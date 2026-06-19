use alloc::vec::Vec;

pub struct ArEntry {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
}

fn parse_name(data: &[u8]) -> Vec<u8> {
    let end = data.iter().position(|&b| b == b'/').unwrap_or(data.len());
    data[..end].to_vec()
}

pub fn parse_ar(input: &[u8]) -> Option<Vec<ArEntry>> {
    if input.len() < 8 || &input[..8] != b"!<arch>\n" {
        return None;
    }
    let mut pos = 8usize;
    let mut entries = Vec::new();

    while pos + 60 <= input.len() {
        let name_raw = &input[pos..pos + 16];
        let size_raw = &input[pos + 48..pos + 58];

        let size: usize = core::str::from_utf8(size_raw)
            .ok()?
            .trim()
            .parse()
            .ok()?;

        let name = parse_name(name_raw);
        let data_pos = pos + 60;
        let data_end = data_pos + size;

        if data_end > input.len() {
            return None;
        }

        let data = input[data_pos..data_end].to_vec();
        entries.push(ArEntry { name, data });

        pos = data_end + (size % 2);
    }

    Some(entries)
}
