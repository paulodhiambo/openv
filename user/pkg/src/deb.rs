use alloc::vec::Vec;

pub struct DebPackage {
    pub package: Vec<u8>,
    pub version: Vec<u8>,
    pub depends: Vec<Vec<u8>>,
    pub filename: Vec<u8>,
    pub description: Vec<u8>,
}

type FieldMap = Vec<(Vec<u8>, Vec<u8>)>;

fn field_get<'a>(fields: &'a FieldMap, key: &[u8]) -> Option<&'a [u8]> {
    for (k, v) in fields.iter() {
        if k.as_slice() == key {
            return Some(v);
        }
    }
    None
}

fn trim_ascii_start(s: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || s[i] == b'\t') {
        i += 1;
    }
    &s[i..]
}

fn trim_ascii(s: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < s.len() && (s[start] == b' ' || s[start] == b'\t' || s[start] == b'\n' || s[start] == b'\r') {
        start += 1;
    }
    let mut end = s.len();
    while end > start && (s[end - 1] == b' ' || s[end - 1] == b'\t' || s[end - 1] == b'\n' || s[end - 1] == b'\r') {
        end -= 1;
    }
    &s[start..end]
}

fn parse_fields(data: &[u8]) -> Option<FieldMap> {
    let mut fields = Vec::new();
    let mut current_key: Option<Vec<u8>> = None;
    let mut current_val: Vec<u8> = Vec::new();

    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }

        if line[0] == b' ' || line[0] == b'\t' {
            let trimmed = trim_ascii_start(line);
            if !current_val.is_empty() {
                current_val.push(b'\n');
            }
            current_val.extend_from_slice(trimmed);
        } else {
            if let Some(ref key) = current_key {
                fields.push((key.clone(), core::mem::take(&mut current_val)));
            }
            let colon = line.iter().position(|&b| b == b':')?;
            let key = line[..colon].iter().map(|&b| b.to_ascii_lowercase()).collect();
            let val = if colon + 1 < line.len() {
                let v = &line[colon + 1..];
                if v.first() == Some(&b' ') { &v[1..] } else { v }
            } else {
                &[]
            };
            current_key = Some(key);
            current_val = val.to_vec();
        }
    }

    if let Some(key) = current_key {
        fields.push((key, current_val));
    }

    Some(fields)
}

pub fn parse_control(data: &[u8]) -> Option<DebPackage> {
    let fields = parse_fields(data)?;

    let package = field_get(&fields, b"package")?;
    let version = field_get(&fields, b"version")?;
    let depends_str = field_get(&fields, b"depends").unwrap_or(&[]);
    let description = field_get(&fields, b"description").unwrap_or(&[]);
    let filename = field_get(&fields, b"filename").unwrap_or(&[]);

    let mut depends = Vec::new();
    for dep in depends_str.split(|&b| b == b',') {
        let dep = trim_ascii(dep);
        let primary = if let Some(pipe) = dep.iter().position(|&b| b == b'|') {
            &dep[..pipe]
        } else {
            dep
        };
        let primary = trim_ascii(primary);
        if !primary.is_empty() {
            let name = if let Some(paren) = primary.iter().position(|&b| b == b'(') {
                &primary[..paren]
            } else {
                primary
            };
            let name = trim_ascii(name);
            if !name.is_empty() {
                depends.push(name.to_vec());
            }
        }
    }

    Some(DebPackage {
        package: package.to_vec(),
        version: version.to_vec(),
        depends,
        filename: filename.to_vec(),
        description: description.to_vec(),
    })
}

pub fn parse_index(data: &[u8]) -> Vec<DebPackage> {
    let mut packages = Vec::new();
    let stanzas = split_stanzas(data);
    for stanza in &stanzas {
        if let Some(pkg) = parse_control(stanza) {
            packages.push(pkg);
        }
    }
    packages
}

fn split_stanzas(data: &[u8]) -> Vec<Vec<u8>> {
    let mut stanzas = Vec::new();
    let mut start = 0usize;

    loop {
        let stanza_end = if let Some(pos) = data[start..].windows(2).position(|w| w == b"\n\n") {
            start + pos
        } else {
            let remaining = trim_ascii(&data[start..]);
            if !remaining.is_empty() {
                stanzas.push(remaining.to_vec());
            }
            break;
        };

        let mut stanza_start = start;
        while stanza_start < data.len() && data[stanza_start] == b'\n' {
            stanza_start += 1;
        }

        if stanza_end > stanza_start {
            stanzas.push(data[stanza_start..stanza_end].to_vec());
        }

        start = stanza_end + 2;
        if start >= data.len() { break; }
    }

    stanzas
}
