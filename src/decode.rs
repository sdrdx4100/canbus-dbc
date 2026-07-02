//! Decode raw CAN frames into physical signal values using a DBC definition.
//!
//! The decoder also owns the output column layout: one column per DBC signal,
//! in DBC declaration order. Signal names that collide across messages are
//! disambiguated as `Message.Signal`. An optional filter restricts the
//! columns (and decoding work) to a chosen subset of signals.

use crate::blf::CanFrame;
use crate::dbc::{ByteOrder, Dbc, MuxRole, SignalDef, ValueType};
use std::collections::{HashMap, HashSet};

/// One output column as derived from the DBC (before any filtering).
#[derive(Debug, Clone)]
pub struct ColumnSpec {
    /// Column name: the signal name, qualified as `Message.Signal` when the
    /// signal name appears in more than one message.
    pub name: String,
    /// Name of the message the signal belongs to.
    pub message: String,
}

/// Column names (with their message) in DBC declaration order. This is what
/// a UI should present for signal selection; pass the chosen `name`s back via
/// `Decoder::with_filter`.
pub fn list_columns(dbc: &Dbc) -> Vec<ColumnSpec> {
    let mut name_count: HashMap<&str, u32> = HashMap::new();
    for message in &dbc.messages {
        for signal in &message.signals {
            *name_count.entry(signal.name.as_str()).or_default() += 1;
        }
    }
    let mut columns = Vec::new();
    for message in &dbc.messages {
        for signal in &message.signals {
            let name = if name_count[signal.name.as_str()] > 1 {
                format!("{}.{}", message.name, signal.name)
            } else {
                signal.name.clone()
            };
            columns.push(ColumnSpec {
                name,
                message: message.name.clone(),
            });
        }
    }
    columns
}

struct BoundSignal {
    def: SignalDef,
    column: usize,
}

struct BoundMessage {
    name: String,
    signals: Vec<BoundSignal>,
    /// Multiplexor switch definition (kept even when its column is filtered
    /// out, since multiplexed signals need the switch value).
    switch: Option<SignalDef>,
}

pub struct Decoder {
    columns: Vec<String>,
    /// Keyed by (id | extended-bit) for O(1) frame lookup.
    messages: HashMap<u32, BoundMessage>,
}

const EXT_KEY_BIT: u32 = 0x8000_0000;

impl Decoder {
    pub fn new(dbc: &Dbc) -> Self {
        Self::with_filter(dbc, None)
    }

    /// Build a decoder whose output is restricted to the given column names
    /// (as produced by [`list_columns`]). `None` exports everything.
    pub fn with_filter(dbc: &Dbc, filter: Option<&HashSet<String>>) -> Self {
        let specs = list_columns(dbc);
        let mut spec_iter = specs.into_iter();

        let mut columns: Vec<String> = Vec::new();
        let mut messages = HashMap::new();
        for message in &dbc.messages {
            let mut bound = BoundMessage {
                name: message.name.clone(),
                signals: Vec::new(),
                switch: None,
            };
            for signal in &message.signals {
                let spec = spec_iter.next().expect("column specs out of sync");
                if signal.mux == MuxRole::Switch {
                    bound.switch = Some(signal.clone());
                }
                let selected = filter.is_none_or(|f| f.contains(&spec.name));
                if selected {
                    bound.signals.push(BoundSignal {
                        def: signal.clone(),
                        column: columns.len(),
                    });
                    columns.push(spec.name);
                }
            }
            let key = message.id | if message.is_extended { EXT_KEY_BIT } else { 0 };
            messages.insert(key, bound);
        }
        Decoder { columns, messages }
    }

    /// Output column names (excluding the leading Timestamp column).
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    fn lookup(&self, frame: &CanFrame) -> Option<&BoundMessage> {
        let key = frame.id | if frame.is_extended { EXT_KEY_BIT } else { 0 };
        self.messages.get(&key)
    }

    /// True when the frame's CAN ID is defined in the DBC.
    pub fn contains_id(&self, frame: &CanFrame) -> bool {
        self.lookup(frame).is_some()
    }

    /// DBC message name for the frame's CAN ID, if defined.
    pub fn message_name(&self, frame: &CanFrame) -> Option<&str> {
        self.lookup(frame).map(|m| m.name.as_str())
    }

    /// Decode a frame into `out` as `(column index, physical value)` pairs.
    /// Returns false if the frame's ID is not in the DBC (or it is a remote
    /// frame carrying no data).
    pub fn decode(&self, frame: &CanFrame, out: &mut Vec<(usize, f64)>) -> bool {
        out.clear();
        if frame.is_remote {
            return false;
        }
        let Some(message) = self.lookup(frame) else {
            return false;
        };
        let data = frame.data();

        let switch_value = message
            .switch
            .as_ref()
            .and_then(|def| extract_raw(data, def));

        for signal in &message.signals {
            match signal.def.mux {
                MuxRole::None | MuxRole::Switch => {}
                MuxRole::Value(v) => {
                    if switch_value != Some(v) {
                        continue;
                    }
                }
            }
            let Some(raw) = extract_raw(data, &signal.def) else {
                continue;
            };
            let value = physical_value(raw, &signal.def);
            out.push((signal.column, value));
        }
        true
    }
}

/// Extract the raw (unscaled) bits of a signal. Returns `None` when the
/// signal does not fit inside the frame payload.
fn extract_raw(data: &[u8], def: &SignalDef) -> Option<u64> {
    let bits = data.len() * 8;
    let start = def.start_bit as usize;
    let length = def.length as usize;
    let mut raw: u64 = 0;
    match def.byte_order {
        ByteOrder::Intel => {
            if start + length > bits {
                return None;
            }
            for i in 0..length {
                let pos = start + i;
                let bit = (data[pos / 8] >> (pos % 8)) & 1;
                raw |= (bit as u64) << i;
            }
        }
        ByteOrder::Motorola => {
            // start_bit is the MSB; walk toward the LSB in big-endian
            // "sawtooth" bit numbering (bit 7 is the MSB of each byte).
            let mut pos = start;
            for _ in 0..length {
                if pos / 8 >= data.len() {
                    return None;
                }
                let bit = (data[pos / 8] >> (pos % 8)) & 1;
                raw = (raw << 1) | bit as u64;
                if pos.is_multiple_of(8) {
                    pos += 15;
                } else {
                    pos -= 1;
                }
            }
        }
    }
    Some(raw)
}

/// Apply sign, IEEE-float reinterpretation, factor and offset.
fn physical_value(raw: u64, def: &SignalDef) -> f64 {
    let base = match def.value_type {
        ValueType::Float if def.length == 32 => f32::from_bits(raw as u32) as f64,
        ValueType::Double if def.length == 64 => f64::from_bits(raw),
        _ => {
            if def.signed {
                sign_extend(raw, def.length) as f64
            } else {
                raw as f64
            }
        }
    };
    base * def.factor + def.offset
}

fn sign_extend(raw: u64, length: u16) -> i64 {
    if length >= 64 {
        return raw as i64;
    }
    let shift = 64 - length as u32;
    ((raw << shift) as i64) >> shift
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbc::Dbc;

    fn frame(id: u32, extended: bool, data: &[u8]) -> CanFrame {
        let mut buf = [0u8; 64];
        buf[..data.len()].copy_from_slice(data);
        CanFrame {
            timestamp_ns: 0,
            channel: 1,
            id,
            is_extended: extended,
            data: buf,
            len: data.len() as u8,
            is_remote: false,
        }
    }

    fn decoder(dbc_text: &str) -> Decoder {
        Decoder::new(&Dbc::parse(dbc_text).unwrap())
    }

    #[test]
    fn intel_unsigned_with_factor() {
        let d = decoder("BO_ 256 M: 8 X\n SG_ Rpm : 0|16@1+ (0.125,0) [0|0] \"\" X\n");
        let mut out = Vec::new();
        // 0x1A20 little-endian = 6688 raw -> 836.0 rpm
        assert!(d.decode(
            &frame(256, false, &[0x20, 0x1A, 0, 0, 0, 0, 0, 0]),
            &mut out
        ));
        assert_eq!(out, vec![(0, 836.0)]);
    }

    #[test]
    fn intel_signed_negative() {
        let d = decoder("BO_ 1 M: 8 X\n SG_ T : 0|8@1- (1,-40) [0|0] \"\" X\n");
        let mut out = Vec::new();
        // raw 0xF6 = -10 signed, minus 40 offset = -50
        d.decode(&frame(1, false, &[0xF6]), &mut out);
        assert_eq!(out, vec![(0, -50.0)]);
    }

    #[test]
    fn motorola_16bit() {
        let d = decoder("BO_ 2 M: 8 X\n SG_ S : 7|16@0+ (1,0) [0|0] \"\" X\n");
        let mut out = Vec::new();
        // Motorola start bit 7 => MSB of byte 0; value = 0x1234
        d.decode(&frame(2, false, &[0x12, 0x34]), &mut out);
        assert_eq!(out, vec![(0, 0x1234 as f64)]);
    }

    #[test]
    fn motorola_signed_crossing_bytes() {
        // 12-bit signed starting at bit 3 of byte 0 (MSB), spans into byte 1.
        let d = decoder("BO_ 3 M: 8 X\n SG_ S : 3|12@0- (0.1,0) [0|0] \"\" X\n");
        let mut out = Vec::new();
        // bits: byte0 low nibble = 0xF, byte1 = 0xFF -> raw 0xFFF = -1 -> -0.1
        d.decode(&frame(3, false, &[0x0F, 0xFF]), &mut out);
        assert_eq!(out.len(), 1);
        assert!((out[0].1 - (-0.1)).abs() < 1e-12);
    }

    #[test]
    fn extended_id_matching() {
        let d = decoder("BO_ 2364540158 E: 8 X\n SG_ P : 16|8@1+ (1,-125) [0|0] \"\" X\n");
        let mut out = Vec::new();
        let id = 2364540158u32 & 0x1FFF_FFFF;
        assert!(d.decode(&frame(id, true, &[0, 0, 200, 0, 0, 0, 0, 0]), &mut out));
        assert_eq!(out, vec![(0, 75.0)]);
        // Same numeric ID as a standard frame must not match.
        assert!(!d.decode(&frame(id, false, &[0; 8]), &mut out));
    }

    #[test]
    fn multiplexed_signals() {
        let text = "BO_ 768 M: 8 X\n SG_ Sw M : 0|4@1+ (1,0) [0|0] \"\" X\n SG_ A m0 : 8|8@1+ (1,0) [0|0] \"\" X\n SG_ B m1 : 8|8@1+ (2,10) [0|0] \"\" X\n";
        let d = decoder(text);
        let mut out = Vec::new();
        d.decode(&frame(768, false, &[0x00, 42]), &mut out);
        assert_eq!(out, vec![(0, 0.0), (1, 42.0)]); // switch + A, not B
        d.decode(&frame(768, false, &[0x01, 42]), &mut out);
        assert_eq!(out, vec![(0, 1.0), (2, 94.0)]); // switch + B (42*2+10)
    }

    #[test]
    fn signal_exceeding_payload_is_skipped() {
        let d = decoder("BO_ 4 M: 8 X\n SG_ S : 32|16@1+ (1,0) [0|0] \"\" X\n");
        let mut out = Vec::new();
        // Frame only has 2 data bytes; the signal needs bits 32..48.
        assert!(d.decode(&frame(4, false, &[1, 2]), &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn float_signal() {
        let text = "BO_ 5 M: 8 X\n SG_ F : 0|32@1- (2,1) [0|0] \"\" X\nSIG_VALTYPE_ 5 F : 1;\n";
        let d = decoder(text);
        let mut out = Vec::new();
        let bytes = 1.5f32.to_le_bytes();
        d.decode(&frame(5, false, &bytes), &mut out);
        assert_eq!(out, vec![(0, 4.0)]); // 1.5 * 2 + 1
    }

    #[test]
    fn duplicate_names_qualified() {
        let text = "BO_ 1 A: 8 X\n SG_ Speed : 0|8@1+ (1,0) [0|0] \"\" X\nBO_ 2 B: 8 X\n SG_ Speed : 0|8@1+ (1,0) [0|0] \"\" X\n SG_ Unique : 8|8@1+ (1,0) [0|0] \"\" X\n";
        let d = decoder(text);
        assert_eq!(d.columns(), &["A.Speed", "B.Speed", "Unique"]);
        let specs = list_columns(&Dbc::parse(text).unwrap());
        assert_eq!(specs[0].message, "A");
        assert_eq!(specs[2].name, "Unique");
    }

    #[test]
    fn signal_filter_restricts_columns() {
        let text = "BO_ 1 M: 8 X\n SG_ A : 0|8@1+ (1,0) [0|0] \"\" X\n SG_ B : 8|8@1+ (1,0) [0|0] \"\" X\n SG_ C : 16|8@1+ (1,0) [0|0] \"\" X\n";
        let dbc = Dbc::parse(text).unwrap();
        let filter: HashSet<String> = ["A", "C"].iter().map(|s| s.to_string()).collect();
        let d = Decoder::with_filter(&dbc, Some(&filter));
        assert_eq!(d.columns(), &["A", "C"]);
        let mut out = Vec::new();
        assert!(d.decode(&frame(1, false, &[1, 2, 3]), &mut out));
        assert_eq!(out, vec![(0, 1.0), (1, 3.0)]);
    }

    #[test]
    fn filtered_mux_values_still_decode() {
        // Switch column not selected, but a multiplexed signal is: the switch
        // must still gate decoding.
        let text = "BO_ 768 M: 8 X\n SG_ Sw M : 0|4@1+ (1,0) [0|0] \"\" X\n SG_ A m0 : 8|8@1+ (1,0) [0|0] \"\" X\n SG_ B m1 : 8|8@1+ (1,0) [0|0] \"\" X\n";
        let dbc = Dbc::parse(text).unwrap();
        let filter: HashSet<String> = ["B"].iter().map(|s| s.to_string()).collect();
        let d = Decoder::with_filter(&dbc, Some(&filter));
        assert_eq!(d.columns(), &["B"]);
        let mut out = Vec::new();
        d.decode(&frame(768, false, &[0x00, 42]), &mut out);
        assert!(out.is_empty()); // switch=0 selects A, B not present
        d.decode(&frame(768, false, &[0x01, 42]), &mut out);
        assert_eq!(out, vec![(0, 42.0)]);
    }

    #[test]
    fn message_name_lookup() {
        let d = decoder("BO_ 256 Engine: 8 X\n SG_ A : 0|8@1+ (1,0) [0|0] \"\" X\n");
        assert_eq!(d.message_name(&frame(256, false, &[0])), Some("Engine"));
        assert_eq!(d.message_name(&frame(257, false, &[0])), None);
        assert!(d.contains_id(&frame(256, false, &[0])));
    }
}
