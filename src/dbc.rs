//! Minimal DBC (CAN database) parser covering what signal decoding needs:
//! `BO_` message definitions, `SG_` signal definitions (byte order, sign,
//! factor/offset, multiplexing) and `SIG_VALTYPE_` float/double overrides.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::Path;

const DBC_ID_EXTENDED_BIT: u32 = 0x8000_0000;
/// Pseudo-message that holds signals not attached to any real frame.
const VECTOR_INDEPENDENT_SIG_MSG: u32 = 0xC000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOrder {
    /// Intel, little-endian (`@1` in DBC).
    Intel,
    /// Motorola, big-endian (`@0` in DBC).
    Motorola,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxRole {
    /// Plain signal, always present.
    None,
    /// The multiplexor switch (`M`).
    Switch,
    /// Present only when the switch equals this value (`m<N>`).
    Value(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Integer,
    Float,
    Double,
}

#[derive(Debug, Clone)]
pub struct SignalDef {
    pub name: String,
    pub start_bit: u16,
    pub length: u16,
    pub byte_order: ByteOrder,
    pub signed: bool,
    pub factor: f64,
    pub offset: f64,
    pub mux: MuxRole,
    pub value_type: ValueType,
    /// Unit string from the DBC (e.g. "rpm"), may be empty.
    pub unit: String,
}

#[derive(Debug, Clone)]
pub struct MessageDef {
    /// Identifier with the extended bit stripped.
    pub id: u32,
    pub is_extended: bool,
    pub name: String,
    pub dlc: u16,
    pub signals: Vec<SignalDef>,
}

#[derive(Debug, Default)]
pub struct Dbc {
    pub messages: Vec<MessageDef>,
    /// True when the file looks like an SAE J1939 database (VFrameFormat /
    /// ProtocolType attributes). J1939 frames should be matched by PGN, not
    /// by the full 29-bit identifier (priority and source address vary).
    pub j1939_hint: bool,
}

impl Dbc {
    pub fn from_file(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read DBC file {}", path.display()))?;
        // DBC files are frequently Latin-1 / CP932 rather than UTF-8; decode
        // lossily since we only need the structural ASCII parts.
        let text = String::from_utf8_lossy(&bytes);
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut messages: Vec<MessageDef> = Vec::new();
        let mut index_of: HashMap<u32, usize> = HashMap::new();
        let mut valtypes: Vec<(u32, String, ValueType)> = Vec::new();
        let mut skipping_orphans = false;

        for (lineno, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim();
            let err_ctx = |what: &str| format!("DBC line {}: {}", lineno + 1, what);

            if let Some(rest) = line.strip_prefix("BO_ ") {
                let raw_id: u32 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|t| t.parse().ok())
                    .with_context(|| err_ctx("invalid message id"))?;
                if raw_id == VECTOR_INDEPENDENT_SIG_MSG {
                    skipping_orphans = true;
                    continue;
                }
                skipping_orphans = false;
                let mut tokens = rest.split_whitespace();
                tokens.next(); // id
                let name = tokens
                    .next()
                    .with_context(|| err_ctx("missing message name"))?
                    .trim_end_matches(':')
                    .to_string();
                let dlc: u16 = tokens
                    .next()
                    .and_then(|t| t.parse().ok())
                    .with_context(|| err_ctx("invalid message DLC"))?;
                let key = raw_id;
                index_of.insert(key, messages.len());
                messages.push(MessageDef {
                    id: raw_id & !DBC_ID_EXTENDED_BIT,
                    is_extended: raw_id & DBC_ID_EXTENDED_BIT != 0,
                    name,
                    dlc,
                    signals: Vec::new(),
                });
            } else if let Some(rest) = line.strip_prefix("SG_ ") {
                if skipping_orphans {
                    continue;
                }
                let Some(message) = messages.last_mut() else {
                    bail!(err_ctx("signal definition before any message"));
                };
                let signal = parse_signal(rest).with_context(|| err_ctx("malformed signal"))?;
                message.signals.push(signal);
            } else if let Some(rest) = line.strip_prefix("SIG_VALTYPE_ ") {
                // SIG_VALTYPE_ <msg id> <signal> : <1|2>;
                let rest = rest.trim_end_matches(';');
                let mut tokens = rest.split_whitespace();
                let (Some(id), Some(name)) = (tokens.next(), tokens.next()) else {
                    continue;
                };
                let ty = match tokens.find(|t| *t != ":") {
                    Some("1") => ValueType::Float,
                    Some("2") => ValueType::Double,
                    _ => continue,
                };
                if let Ok(id) = id.parse::<u32>() {
                    valtypes.push((id, name.to_string(), ty));
                }
            }
        }

        for (raw_id, name, ty) in valtypes {
            if let Some(&idx) = index_of.get(&raw_id) {
                for signal in &mut messages[idx].signals {
                    if signal.name == name {
                        signal.value_type = ty;
                    }
                }
            }
        }

        Ok(Dbc {
            messages,
            j1939_hint: text.contains("J1939"),
        })
    }
}

/// Parse the part of an `SG_` line after the keyword:
/// `<name> [M|m<N>] : <start>|<len>@<order><sign> (<factor>,<offset>) [...] "unit" receivers`
fn parse_signal(rest: &str) -> Result<SignalDef> {
    let (head, tail) = rest.split_once(':').context("missing ':' separator")?;
    let mut head_tokens = head.split_whitespace();
    let name = head_tokens
        .next()
        .context("missing signal name")?
        .to_string();
    let mux = match head_tokens.next() {
        None => MuxRole::None,
        Some("M") => MuxRole::Switch,
        Some(tok) => {
            let tok = tok.trim_end_matches('M'); // extended mux: "m2M"
            let value = tok
                .strip_prefix('m')
                .and_then(|v| v.parse().ok())
                .context("invalid multiplex indicator")?;
            MuxRole::Value(value)
        }
    };

    let mut tail_tokens = tail.split_whitespace();
    let placement = tail_tokens.next().context("missing bit placement")?;
    // <start>|<len>@<order><sign>
    let (start, rest) = placement.split_once('|').context("bad bit placement")?;
    let (len, order_sign) = rest.split_once('@').context("bad bit placement")?;
    let start_bit: u16 = start.parse().context("bad start bit")?;
    let length: u16 = len.parse().context("bad signal length")?;
    if length == 0 || length > 64 {
        bail!("signal length {length} out of range");
    }
    let mut chars = order_sign.chars();
    let byte_order = match chars.next() {
        Some('1') => ByteOrder::Intel,
        Some('0') => ByteOrder::Motorola,
        _ => bail!("bad byte order"),
    };
    let signed = match chars.next() {
        Some('+') => false,
        Some('-') => true,
        _ => bail!("bad sign indicator"),
    };

    let factor_offset = tail_tokens.next().context("missing factor/offset")?;
    let inner = factor_offset
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .context("bad factor/offset")?;
    let (factor, offset) = inner.split_once(',').context("bad factor/offset")?;
    let factor: f64 = factor.parse().context("bad factor")?;
    let offset: f64 = offset.parse().context("bad offset")?;

    // Unit: the first quoted string after the factor/offset parentheses,
    // e.g. `[0|8031.875] "rpm" Vector__XXX`. Missing or empty is fine.
    let unit = tail
        .split_once(')')
        .map(|(_, after)| after)
        .and_then(|after| {
            let start = after.find('"')? + 1;
            let end = start + after[start..].find('"')?;
            Some(after[start..end].to_string())
        })
        .unwrap_or_default();

    Ok(SignalDef {
        name,
        start_bit,
        length,
        byte_order,
        signed,
        factor,
        offset,
        mux,
        value_type: ValueType::Integer,
        unit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
VERSION ""

BO_ 256 Engine: 8 ECU1
 SG_ EngineSpeed : 0|16@1+ (0.125,0) [0|8000] "rpm" Vector__XXX
 SG_ EngineTemp : 16|8@1- (1,-40) [-40|215] "degC" Vector__XXX

BO_ 2364540158 EEC1: 8 Vector__XXX
 SG_ ActualEnginePercentTorque : 16|8@1+ (1,-125) [-125|125] "%" Vector__XXX

BO_ 512 Steering: 8 ECU2
 SG_ SteeringAngle : 7|16@0- (0.1,0) [-780|779.9] "deg" Vector__XXX

BO_ 768 MuxMsg: 8 ECU3
 SG_ MuxSwitch M : 0|4@1+ (1,0) [0|15] "" Vector__XXX
 SG_ ValueA m0 : 8|8@1+ (1,0) [0|255] "" Vector__XXX
 SG_ ValueB m1 : 8|8@1+ (2,10) [0|255] "" Vector__XXX

BO_ 3221225472 VECTOR__INDEPENDENT_SIG_MSG: 0 Vector__XXX
 SG_ Orphan : 0|8@1+ (1,0) [0|0] "" Vector__XXX

BO_ 1024 FloatMsg: 8 ECU4
 SG_ FloatSig : 0|32@1- (1,0) [0|0] "" Vector__XXX
SIG_VALTYPE_ 1024 FloatSig : 1;
"#;

    #[test]
    fn parses_messages_and_signals() {
        let dbc = Dbc::parse(SAMPLE).unwrap();
        assert_eq!(dbc.messages.len(), 5);

        let engine = &dbc.messages[0];
        assert_eq!(engine.id, 256);
        assert!(!engine.is_extended);
        assert_eq!(engine.name, "Engine");
        assert_eq!(engine.signals.len(), 2);
        let rpm = &engine.signals[0];
        assert_eq!(rpm.name, "EngineSpeed");
        assert_eq!((rpm.start_bit, rpm.length), (0, 16));
        assert_eq!(rpm.byte_order, ByteOrder::Intel);
        assert!(!rpm.signed);
        assert_eq!(rpm.factor, 0.125);
        let temp = &engine.signals[1];
        assert!(temp.signed);
        assert_eq!(temp.offset, -40.0);

        let eec1 = &dbc.messages[1];
        assert!(eec1.is_extended);
        assert_eq!(eec1.id, 2364540158 & 0x1FFF_FFFF);

        let steering = &dbc.messages[2];
        assert_eq!(steering.signals[0].byte_order, ByteOrder::Motorola);

        let mux = &dbc.messages[3];
        assert_eq!(mux.signals[0].mux, MuxRole::Switch);
        assert_eq!(mux.signals[1].mux, MuxRole::Value(0));
        assert_eq!(mux.signals[2].mux, MuxRole::Value(1));

        let float_msg = &dbc.messages[4];
        assert_eq!(float_msg.signals[0].value_type, ValueType::Float);
    }

    #[test]
    fn parses_units_and_j1939_hint() {
        let dbc = Dbc::parse(SAMPLE).unwrap();
        assert_eq!(dbc.messages[0].signals[0].unit, "rpm");
        assert_eq!(dbc.messages[0].signals[1].unit, "degC");
        assert_eq!(dbc.messages[1].signals[0].unit, "%");
        assert!(!dbc.j1939_hint);

        let j1939 = Dbc::parse(
            "BA_DEF_ BO_ \"VFrameFormat\" ENUM \"StandardCAN\",\"J1939PG\";\nBO_ 2364540158 EEC1: 8 X\n SG_ EngSpeed : 24|16@1+ (0.125,0) [0|8031.875] \"rpm\" X\n",
        )
        .unwrap();
        assert!(j1939.j1939_hint);
    }

    #[test]
    fn skips_orphan_signals() {
        let dbc = Dbc::parse(SAMPLE).unwrap();
        assert!(
            dbc.messages
                .iter()
                .all(|m| m.signals.iter().all(|s| s.name != "Orphan"))
        );
    }
}
