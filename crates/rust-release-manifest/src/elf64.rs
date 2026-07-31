// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{error::Error, fmt};

const MAX_INPUT: usize = 256 * 1024 * 1024;
const MAX_PROGRAM_HEADERS: usize = 4096;
const MAX_DYNAMIC_ENTRIES: usize = 65_536;
const MAX_STRING_TABLE: usize = 16 * 1024 * 1024;
const MAX_STRING: usize = 4096;
const MAX_NEEDED: usize = 128;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_STRTAB: u64 = 5;
const DT_STRSZ: u64 = 10;
const DT_SONAME: u64 = 14;
const DT_RPATH: u64 = 15;
const DT_RUNPATH: u64 = 29;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Elf64Linkage {
    pub(crate) elf_type: u16,
    pub(crate) machine: u16,
    pub(crate) interpreter: String,
    pub(crate) needed: Vec<String>,
    pub(crate) soname: Option<String>,
    pub(crate) rpath: Option<String>,
    pub(crate) runpath: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Elf64Error {
    TooLarge {
        actual: usize,
        limit: usize,
    },
    Truncated {
        region: &'static str,
        offset: u64,
    },
    IntegerOverflow {
        region: &'static str,
        index: usize,
        offset: u64,
    },
    BadMagic,
    UnsupportedClass {
        actual: u8,
    },
    UnsupportedEndian {
        actual: u8,
    },
    UnsupportedIdentVersion {
        actual: u8,
    },
    UnsupportedType {
        actual: u16,
    },
    UnsupportedMachine {
        actual: u16,
    },
    InvalidHeaderSize {
        actual: u16,
    },
    InvalidProgramHeaderSize {
        actual: u16,
    },
    ProgramHeaderCountExceeded {
        actual: usize,
        limit: usize,
    },
    ProgramHeaderOutOfBounds {
        index: usize,
        offset: u64,
    },
    SegmentOutOfBounds {
        region: &'static str,
        index: usize,
        offset: u64,
    },
    MissingLoadSegment,
    MissingInterpreter,
    DuplicateInterpreter {
        index: usize,
    },
    InterpreterNotTerminated {
        offset: u64,
    },
    InterpreterNotUtf8 {
        offset: u64,
    },
    MissingDynamic,
    DuplicateDynamic {
        index: usize,
    },
    DynamicEntryCountExceeded {
        actual: usize,
        limit: usize,
    },
    DynamicNotTerminated {
        offset: u64,
    },
    MissingStringTable,
    DuplicateStringTable {
        index: usize,
    },
    MissingStringTableSize,
    DuplicateStringTableSize {
        index: usize,
    },
    StringTableAddressUnmapped {
        offset: u64,
    },
    StringTableOutOfBounds {
        offset: u64,
    },
    StringOffsetOutOfBounds {
        tag: u64,
        index: usize,
        offset: u64,
    },
    StringNotTerminated {
        tag: u64,
        index: usize,
        offset: u64,
    },
    StringNotUtf8 {
        tag: u64,
        index: usize,
        offset: u64,
    },
    StringTooLong {
        tag: u64,
        index: usize,
        offset: u64,
    },
    NeededCountExceeded {
        actual: usize,
        limit: usize,
    },
    DuplicateSingletonTag {
        tag: u64,
        index: usize,
    },
}

impl fmt::Display for Elf64Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ELF64 parse failed: {self:?}")
    }
}

impl Error for Elf64Error {}

#[derive(Clone, Copy)]
struct LoadSegment {
    file_offset: u64,
    virtual_address: u64,
    file_size: u64,
}

#[derive(Clone, Copy)]
struct Segment {
    index: usize,
    offset: u64,
    size: u64,
}

fn bytes<'a>(
    input: &'a [u8],
    offset: u64,
    size: usize,
    region: &'static str,
) -> Result<&'a [u8], Elf64Error> {
    let start = usize::try_from(offset).map_err(|_| Elf64Error::IntegerOverflow {
        region,
        index: 0,
        offset,
    })?;
    let end = start.checked_add(size).ok_or(Elf64Error::IntegerOverflow {
        region,
        index: 0,
        offset,
    })?;
    input
        .get(start..end)
        .ok_or(Elf64Error::Truncated { region, offset })
}

fn u16_at(input: &[u8], offset: u64, region: &'static str) -> Result<u16, Elf64Error> {
    let value: [u8; 2] = bytes(input, offset, 2, region)?
        .try_into()
        .map_err(|_| Elf64Error::Truncated { region, offset })?;
    Ok(u16::from_le_bytes(value))
}

fn u32_at(input: &[u8], offset: u64, region: &'static str) -> Result<u32, Elf64Error> {
    let value: [u8; 4] = bytes(input, offset, 4, region)?
        .try_into()
        .map_err(|_| Elf64Error::Truncated { region, offset })?;
    Ok(u32::from_le_bytes(value))
}

fn u64_at(input: &[u8], offset: u64, region: &'static str) -> Result<u64, Elf64Error> {
    let value: [u8; 8] = bytes(input, offset, 8, region)?
        .try_into()
        .map_err(|_| Elf64Error::Truncated { region, offset })?;
    Ok(u64::from_le_bytes(value))
}

fn segment_slice<'a>(
    input: &'a [u8],
    segment: Segment,
    region: &'static str,
) -> Result<&'a [u8], Elf64Error> {
    let size = usize::try_from(segment.size).map_err(|_| Elf64Error::IntegerOverflow {
        region,
        index: segment.index,
        offset: segment.offset,
    })?;
    bytes(input, segment.offset, size, region).map_err(|error| match error {
        Elf64Error::Truncated { .. } => Elf64Error::SegmentOutOfBounds {
            region,
            index: segment.index,
            offset: segment.offset,
        },
        other => other,
    })
}

pub(crate) fn parse_elf64(input: &[u8]) -> Result<Elf64Linkage, Elf64Error> {
    if input.len() > MAX_INPUT {
        return Err(Elf64Error::TooLarge {
            actual: input.len(),
            limit: MAX_INPUT,
        });
    }
    let ident = bytes(input, 0, 16, "ident")?;
    if ident.get(0..4) != Some(b"\x7fELF") {
        return Err(Elf64Error::BadMagic);
    }
    if ident[4] != 2 {
        return Err(Elf64Error::UnsupportedClass { actual: ident[4] });
    }
    if ident[5] != 1 {
        return Err(Elf64Error::UnsupportedEndian { actual: ident[5] });
    }
    if ident[6] != 1 {
        return Err(Elf64Error::UnsupportedIdentVersion { actual: ident[6] });
    }
    let elf_type = u16_at(input, 16, "header")?;
    if elf_type != 3 {
        return Err(Elf64Error::UnsupportedType { actual: elf_type });
    }
    let machine = u16_at(input, 18, "header")?;
    if machine != 62 {
        return Err(Elf64Error::UnsupportedMachine { actual: machine });
    }
    let header_size = u16_at(input, 52, "header")?;
    if header_size != 64 {
        return Err(Elf64Error::InvalidHeaderSize {
            actual: header_size,
        });
    }
    let program_offset = u64_at(input, 32, "header")?;
    let program_size = u16_at(input, 54, "header")?;
    if program_size != 56 {
        return Err(Elf64Error::InvalidProgramHeaderSize {
            actual: program_size,
        });
    }
    let program_count = usize::from(u16_at(input, 56, "header")?);
    if program_count > MAX_PROGRAM_HEADERS {
        return Err(Elf64Error::ProgramHeaderCountExceeded {
            actual: program_count,
            limit: MAX_PROGRAM_HEADERS,
        });
    }
    let table_size = program_count.checked_mul(usize::from(program_size)).ok_or(
        Elf64Error::IntegerOverflow {
            region: "program-headers",
            index: program_count,
            offset: program_offset,
        },
    )?;
    bytes(input, program_offset, table_size, "program-headers").map_err(|_| {
        Elf64Error::ProgramHeaderOutOfBounds {
            index: program_count,
            offset: program_offset,
        }
    })?;

    let mut loads = Vec::new();
    let mut interpreter = None;
    let mut dynamic = None;
    for index in 0..program_count {
        let relative = index.checked_mul(56).ok_or(Elf64Error::IntegerOverflow {
            region: "program-header",
            index,
            offset: program_offset,
        })?;
        let offset = program_offset
            .checked_add(
                u64::try_from(relative).map_err(|_| Elf64Error::IntegerOverflow {
                    region: "program-header",
                    index,
                    offset: program_offset,
                })?,
            )
            .ok_or(Elf64Error::IntegerOverflow {
                region: "program-header",
                index,
                offset: program_offset,
            })?;
        let kind = u32_at(input, offset, "program-header")?;
        let file_offset = u64_at(input, offset + 8, "program-header")?;
        let virtual_address = u64_at(input, offset + 16, "program-header")?;
        let file_size = u64_at(input, offset + 32, "program-header")?;
        let segment = Segment {
            index,
            offset: file_offset,
            size: file_size,
        };
        match kind {
            PT_LOAD => {
                segment_slice(input, segment, "load")?;
                loads.push(LoadSegment {
                    file_offset,
                    virtual_address,
                    file_size,
                });
            }
            PT_INTERP => {
                if interpreter.is_some() {
                    return Err(Elf64Error::DuplicateInterpreter { index });
                }
                let value = segment_slice(input, segment, "interpreter")?;
                let Some(body) = value.strip_suffix(&[0]) else {
                    return Err(Elf64Error::InterpreterNotTerminated {
                        offset: file_offset,
                    });
                };
                if body.contains(&0) {
                    return Err(Elf64Error::InterpreterNotTerminated {
                        offset: file_offset,
                    });
                }
                interpreter = Some(
                    std::str::from_utf8(body)
                        .map_err(|_| Elf64Error::InterpreterNotUtf8 {
                            offset: file_offset,
                        })?
                        .to_owned(),
                );
            }
            PT_DYNAMIC => {
                if dynamic.is_some() {
                    return Err(Elf64Error::DuplicateDynamic { index });
                }
                segment_slice(input, segment, "dynamic")?;
                dynamic = Some(segment);
            }
            _ => {}
        }
    }
    if loads.is_empty() {
        return Err(Elf64Error::MissingLoadSegment);
    }
    let interpreter = interpreter.ok_or(Elf64Error::MissingInterpreter)?;
    let dynamic = dynamic.ok_or(Elf64Error::MissingDynamic)?;
    let dynamic_bytes = segment_slice(input, dynamic, "dynamic")?;
    let entry_count = dynamic_bytes.len() / 16;
    if entry_count > MAX_DYNAMIC_ENTRIES {
        return Err(Elf64Error::DynamicEntryCountExceeded {
            actual: entry_count,
            limit: MAX_DYNAMIC_ENTRIES,
        });
    }

    let mut string_address = None;
    let mut string_size = None;
    let mut strings = Vec::new();
    let mut terminated = false;
    for index in 0..entry_count {
        let offset = u64::try_from(index * 16).map_err(|_| Elf64Error::IntegerOverflow {
            region: "dynamic",
            index,
            offset: dynamic.offset,
        })?;
        let tag = u64_at(dynamic_bytes, offset, "dynamic-entry")?;
        let value = u64_at(dynamic_bytes, offset + 8, "dynamic-entry")?;
        if tag == DT_NULL {
            terminated = true;
            break;
        }
        match tag {
            DT_STRTAB => {
                if string_address.replace(value).is_some() {
                    return Err(Elf64Error::DuplicateStringTable { index });
                }
            }
            DT_STRSZ => {
                if string_size.replace(value).is_some() {
                    return Err(Elf64Error::DuplicateStringTableSize { index });
                }
            }
            DT_NEEDED | DT_SONAME | DT_RPATH | DT_RUNPATH => {
                if tag != DT_NEEDED && strings.iter().any(|(prior, _, _)| *prior == tag) {
                    return Err(Elf64Error::DuplicateSingletonTag { tag, index });
                }
                strings.push((tag, index, value));
            }
            _ => {}
        }
    }
    if !terminated {
        return Err(Elf64Error::DynamicNotTerminated {
            offset: dynamic.offset,
        });
    }
    let string_address = string_address.ok_or(Elf64Error::MissingStringTable)?;
    let string_size = string_size.ok_or(Elf64Error::MissingStringTableSize)?;
    let string_size_usize =
        usize::try_from(string_size).map_err(|_| Elf64Error::StringTableOutOfBounds {
            offset: string_address,
        })?;
    if string_size_usize > MAX_STRING_TABLE {
        return Err(Elf64Error::StringTableOutOfBounds {
            offset: string_address,
        });
    }
    let string_offset = loads.iter().find_map(|load| {
        let end = load.virtual_address.checked_add(load.file_size)?;
        if string_address < load.virtual_address || string_address >= end {
            return None;
        }
        load.file_offset
            .checked_add(string_address - load.virtual_address)
    });
    let string_offset = string_offset.ok_or(Elf64Error::StringTableAddressUnmapped {
        offset: string_address,
    })?;
    let string_table =
        bytes(input, string_offset, string_size_usize, "string-table").map_err(|_| {
            Elf64Error::StringTableOutOfBounds {
                offset: string_offset,
            }
        })?;
    let needed_count = strings
        .iter()
        .filter(|(tag, _, _)| *tag == DT_NEEDED)
        .count();
    if needed_count > MAX_NEEDED {
        return Err(Elf64Error::NeededCountExceeded {
            actual: needed_count,
            limit: MAX_NEEDED,
        });
    }

    let mut needed = Vec::new();
    let mut soname = None;
    let mut rpath = None;
    let mut runpath = None;
    for (tag, index, offset) in strings {
        let start = usize::try_from(offset).map_err(|_| Elf64Error::StringOffsetOutOfBounds {
            tag,
            index,
            offset,
        })?;
        let tail = string_table
            .get(start..)
            .ok_or(Elf64Error::StringOffsetOutOfBounds { tag, index, offset })?;
        let end = tail
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(Elf64Error::StringNotTerminated { tag, index, offset })?;
        if end > MAX_STRING {
            return Err(Elf64Error::StringTooLong { tag, index, offset });
        }
        let value = std::str::from_utf8(&tail[..end])
            .map_err(|_| Elf64Error::StringNotUtf8 { tag, index, offset })?
            .to_owned();
        match tag {
            DT_NEEDED => needed.push(value),
            DT_SONAME => soname = Some(value),
            DT_RPATH => rpath = Some(value),
            DT_RUNPATH => runpath = Some(value),
            _ => unreachable!(),
        }
    }
    Ok(Elf64Linkage {
        elf_type,
        machine,
        interpreter,
        needed,
        soname,
        rpath,
        runpath,
    })
}

#[cfg(test)]
pub(crate) fn pinned_elf64_for_test() -> Vec<u8> {
    linkage_elf64_for_test(
        "/lib64/ld-linux-x86-64.so.2",
        &[
            "libgstreamer-1.0.so.0",
            "libgobject-2.0.so.0",
            "libglib-2.0.so.0",
            "libgio-2.0.so.0",
            "libpulse.so.0",
            "libgcc_s.so.1",
            "libm.so.6",
            "libc.so.6",
        ],
        None,
        None,
        None,
    )
}

#[cfg(test)]
pub(crate) fn linkage_elf64_for_test(
    interpreter_value: &str,
    libraries: &[&str],
    soname: Option<&str>,
    rpath: Option<&str>,
    runpath: Option<&str>,
) -> Vec<u8> {
    const PH: usize = 64;
    const INTERP: usize = 256;
    const DYNAMIC: usize = 320;
    const STRINGS: usize = 600;
    const BASE: u64 = 0x400000;
    let mut interpreter = interpreter_value.as_bytes().to_vec();
    interpreter.push(0);
    let mut table = vec![0];
    let mut offsets = Vec::new();
    for library in libraries {
        offsets.push(table.len() as u64);
        table.extend_from_slice(library.as_bytes());
        table.push(0);
    }
    let mut singleton = Vec::new();
    for (tag, value) in [
        (DT_SONAME, soname),
        (DT_RPATH, rpath),
        (DT_RUNPATH, runpath),
    ] {
        if let Some(value) = value {
            let offset = table.len() as u64;
            table.extend_from_slice(value.as_bytes());
            table.push(0);
            singleton.push((tag, offset));
        }
    }
    let mut bytes = vec![0_u8; STRINGS + table.len()];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&3_u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
    bytes[32..40].copy_from_slice(&(PH as u64).to_le_bytes());
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&3_u16.to_le_bytes());
    let put_u32 = |bytes: &mut [u8], offset: usize, value: u32| {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    let put_u64 = |bytes: &mut [u8], offset: usize, value: u64| {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    };
    put_u32(&mut bytes, PH, PT_LOAD);
    put_u64(&mut bytes, PH + 16, BASE);
    let total = bytes.len() as u64;
    put_u64(&mut bytes, PH + 32, total);
    put_u64(&mut bytes, PH + 40, total);
    put_u32(&mut bytes, PH + 56, PT_INTERP);
    put_u64(&mut bytes, PH + 64, INTERP as u64);
    put_u64(&mut bytes, PH + 88, interpreter.len() as u64);
    put_u32(&mut bytes, PH + 112, PT_DYNAMIC);
    put_u64(&mut bytes, PH + 120, DYNAMIC as u64);
    put_u64(
        &mut bytes,
        PH + 144,
        ((offsets.len() + singleton.len() + 3) * 16) as u64,
    );
    bytes[INTERP..INTERP + interpreter.len()].copy_from_slice(&interpreter);
    let mut entries = vec![
        (DT_STRTAB, BASE + STRINGS as u64),
        (DT_STRSZ, table.len() as u64),
    ];
    entries.extend(offsets.into_iter().map(|offset| (DT_NEEDED, offset)));
    entries.extend(singleton);
    entries.push((DT_NULL, 0));
    for (index, (tag, value)) in entries.into_iter().enumerate() {
        put_u64(&mut bytes, DYNAMIC + index * 16, tag);
        put_u64(&mut bytes, DYNAMIC + index * 16 + 8, value);
    }
    bytes[STRINGS..].copy_from_slice(&table);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[derive(Clone, Copy, Default)]
    struct MinimalElf64Options;

    fn minimal_elf64(_options: MinimalElf64Options) -> Vec<u8> {
        const PH: usize = 64;
        const INTERP: usize = 512;
        const DYNAMIC: usize = 600;
        const STRINGS: usize = 800;
        const BASE: u64 = 0x400000;
        let interpreter = b"/lib64/ld-linux-x86-64.so.2\0";
        let table = b"\0libc.so.6\0fixture.so\0/rpath\0/runpath\0";
        let mut bytes = vec![0_u8; STRINGS + table.len()];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        put_u16(&mut bytes, 16, 3);
        put_u16(&mut bytes, 18, 62);
        put_u32(&mut bytes, 20, 1);
        put_u64(&mut bytes, 32, PH as u64);
        put_u16(&mut bytes, 52, 64);
        put_u16(&mut bytes, 54, 56);
        put_u16(&mut bytes, 56, 3);
        put_u32(&mut bytes, PH, PT_LOAD);
        put_u64(&mut bytes, PH + 8, 0);
        put_u64(&mut bytes, PH + 16, BASE);
        let total = bytes.len() as u64;
        put_u64(&mut bytes, PH + 32, total);
        put_u64(&mut bytes, PH + 40, total);
        put_u32(&mut bytes, PH + 56, PT_INTERP);
        put_u64(&mut bytes, PH + 64, INTERP as u64);
        put_u64(&mut bytes, PH + 88, interpreter.len() as u64);
        put_u32(&mut bytes, PH + 112, PT_DYNAMIC);
        put_u64(&mut bytes, PH + 120, DYNAMIC as u64);
        put_u64(&mut bytes, PH + 144, 6 * 16);
        bytes[INTERP..INTERP + interpreter.len()].copy_from_slice(interpreter);
        for (index, (tag, value)) in [
            (DT_STRTAB, BASE + STRINGS as u64),
            (DT_STRSZ, table.len() as u64),
            (DT_NEEDED, 1),
            (DT_SONAME, 11),
            (DT_RPATH, 22),
            (DT_NULL, 0),
        ]
        .into_iter()
        .enumerate()
        {
            put_u64(&mut bytes, DYNAMIC + index * 16, tag);
            put_u64(&mut bytes, DYNAMIC + index * 16 + 8, value);
        }
        bytes[STRINGS..].copy_from_slice(table);
        bytes
    }

    #[test]
    fn parses_byte_exact_minimal_image() {
        let parsed = parse_elf64(&minimal_elf64(MinimalElf64Options)).unwrap();
        assert_eq!(parsed.elf_type, 3);
        assert_eq!(parsed.machine, 62);
        assert_eq!(parsed.interpreter, "/lib64/ld-linux-x86-64.so.2");
        assert_eq!(parsed.needed, ["libc.so.6"]);
        assert_eq!(parsed.soname.as_deref(), Some("fixture.so"));
        assert_eq!(parsed.rpath.as_deref(), Some("/rpath"));
        assert_eq!(parsed.runpath, None);
    }

    #[test]
    fn error_taxonomy_has_a_byte_exact_mutation_per_variant() {
        const PH: usize = 64;
        const INTERP: usize = 512;
        const DYNAMIC: usize = 600;
        const STRINGS: usize = 800;
        const BASE: u64 = 0x400000;

        let clean = minimal_elf64(MinimalElf64Options);
        let check = |name: &str, bytes: Vec<u8>, expected: Elf64Error| {
            assert_eq!(parse_elf64(&bytes), Err(expected), "{name}");
        };

        check(
            "input limit",
            vec![0; MAX_INPUT + 1],
            Elf64Error::TooLarge {
                actual: MAX_INPUT + 1,
                limit: MAX_INPUT,
            },
        );
        check(
            "ident truncation",
            clean[..15].to_vec(),
            Elf64Error::Truncated {
                region: "ident",
                offset: 0,
            },
        );
        check(
            "header truncation",
            clean[..17].to_vec(),
            Elf64Error::Truncated {
                region: "header",
                offset: 16,
            },
        );

        let mut bytes = clean.clone();
        put_u64(&mut bytes, PH + 56 + 8, u64::MAX);
        put_u64(&mut bytes, PH + 56 + 32, 2);
        check(
            "checked segment offset overflow",
            bytes,
            Elf64Error::IntegerOverflow {
                region: "interpreter",
                index: 0,
                offset: u64::MAX,
            },
        );

        for (name, offset, value, expected) in [
            ("magic", 0, 0, Elf64Error::BadMagic),
            ("class", 4, 1, Elf64Error::UnsupportedClass { actual: 1 }),
            ("endian", 5, 2, Elf64Error::UnsupportedEndian { actual: 2 }),
            (
                "ident version",
                6,
                2,
                Elf64Error::UnsupportedIdentVersion { actual: 2 },
            ),
        ] {
            let mut bytes = clean.clone();
            bytes[offset] = value;
            check(name, bytes, expected);
        }
        for (name, offset, value, expected) in [
            ("ET_EXEC", 16, 2, Elf64Error::UnsupportedType { actual: 2 }),
            (
                "machine",
                18,
                3,
                Elf64Error::UnsupportedMachine { actual: 3 },
            ),
            (
                "header size",
                52,
                63,
                Elf64Error::InvalidHeaderSize { actual: 63 },
            ),
            (
                "program header size",
                54,
                55,
                Elf64Error::InvalidProgramHeaderSize { actual: 55 },
            ),
        ] {
            let mut bytes = clean.clone();
            put_u16(&mut bytes, offset, value);
            check(name, bytes, expected);
        }

        let mut bytes = clean.clone();
        put_u16(&mut bytes, 56, 4097);
        check(
            "program header count",
            bytes,
            Elf64Error::ProgramHeaderCountExceeded {
                actual: 4097,
                limit: MAX_PROGRAM_HEADERS,
            },
        );
        let mut bytes = clean.clone();
        put_u64(&mut bytes, 32, u64::MAX);
        check(
            "program header table offset",
            bytes,
            Elf64Error::ProgramHeaderOutOfBounds {
                index: 3,
                offset: u64::MAX,
            },
        );
        let mut bytes = clean.clone();
        let beyond_file = bytes.len() as u64 + 1;
        put_u64(&mut bytes, PH + 32, beyond_file);
        check(
            "load segment boundary",
            bytes,
            Elf64Error::SegmentOutOfBounds {
                region: "load",
                index: 0,
                offset: 0,
            },
        );

        let mut bytes = clean.clone();
        put_u32(&mut bytes, PH, 0);
        check("missing load", bytes, Elf64Error::MissingLoadSegment);
        let mut bytes = clean.clone();
        put_u32(&mut bytes, PH + 56, 0);
        check("missing interpreter", bytes, Elf64Error::MissingInterpreter);
        let mut bytes = clean.clone();
        put_u16(&mut bytes, 56, 4);
        bytes.copy_within(PH + 56..PH + 112, PH + 168);
        check(
            "duplicate interpreter",
            bytes,
            Elf64Error::DuplicateInterpreter { index: 3 },
        );
        let mut bytes = clean.clone();
        let interpreter_size =
            usize::try_from(u64_at(&bytes, (PH + 56 + 32) as u64, "test").unwrap()).unwrap();
        bytes[INTERP + interpreter_size - 1] = b'x';
        check(
            "interpreter terminator",
            bytes,
            Elf64Error::InterpreterNotTerminated {
                offset: INTERP as u64,
            },
        );
        let mut bytes = clean.clone();
        bytes[INTERP] = 0xff;
        check(
            "interpreter UTF-8",
            bytes,
            Elf64Error::InterpreterNotUtf8 {
                offset: INTERP as u64,
            },
        );

        let mut bytes = clean.clone();
        put_u32(&mut bytes, PH + 112, 0);
        check("missing dynamic", bytes, Elf64Error::MissingDynamic);
        let mut bytes = clean.clone();
        put_u16(&mut bytes, 56, 4);
        bytes.copy_within(PH + 112..PH + 168, PH + 168);
        check(
            "duplicate dynamic",
            bytes,
            Elf64Error::DuplicateDynamic { index: 3 },
        );
        let mut bytes = clean.clone();
        bytes.resize(DYNAMIC + (MAX_DYNAMIC_ENTRIES + 1) * 16, 0);
        let total = bytes.len() as u64;
        put_u64(&mut bytes, PH + 32, total);
        put_u64(
            &mut bytes,
            PH + 112 + 32,
            ((MAX_DYNAMIC_ENTRIES + 1) * 16) as u64,
        );
        check(
            "dynamic entry count",
            bytes,
            Elf64Error::DynamicEntryCountExceeded {
                actual: MAX_DYNAMIC_ENTRIES + 1,
                limit: MAX_DYNAMIC_ENTRIES,
            },
        );
        let mut bytes = clean.clone();
        put_u64(&mut bytes, DYNAMIC + 5 * 16, 0xfeed);
        check(
            "dynamic terminator",
            bytes,
            Elf64Error::DynamicNotTerminated {
                offset: DYNAMIC as u64,
            },
        );

        let mut bytes = clean.clone();
        put_u64(&mut bytes, DYNAMIC, 0xfeed);
        check(
            "missing string table",
            bytes,
            Elf64Error::MissingStringTable,
        );
        let mut bytes = clean.clone();
        put_u64(&mut bytes, DYNAMIC + 4 * 16, DT_STRTAB);
        check(
            "duplicate string table",
            bytes,
            Elf64Error::DuplicateStringTable { index: 4 },
        );
        let mut bytes = clean.clone();
        put_u64(&mut bytes, DYNAMIC + 16, 0xfeed);
        check(
            "missing string table size",
            bytes,
            Elf64Error::MissingStringTableSize,
        );
        let mut bytes = clean.clone();
        put_u64(&mut bytes, DYNAMIC + 4 * 16, DT_STRSZ);
        check(
            "duplicate string table size",
            bytes,
            Elf64Error::DuplicateStringTableSize { index: 4 },
        );
        let mut bytes = clean.clone();
        let unmapped = BASE + bytes.len() as u64 + 1;
        put_u64(&mut bytes, DYNAMIC + 8, unmapped);
        check(
            "unmapped string table",
            bytes,
            Elf64Error::StringTableAddressUnmapped {
                offset: BASE + clean.len() as u64 + 1,
            },
        );
        let mut bytes = clean.clone();
        put_u64(&mut bytes, DYNAMIC + 16 + 8, (MAX_STRING_TABLE + 1) as u64);
        check(
            "string table limit",
            bytes,
            Elf64Error::StringTableOutOfBounds {
                offset: BASE + STRINGS as u64,
            },
        );
        let mut bytes = clean.clone();
        put_u64(
            &mut bytes,
            DYNAMIC + 16 + 8,
            (clean.len() - STRINGS + 1) as u64,
        );
        check(
            "string table file boundary",
            bytes,
            Elf64Error::StringTableOutOfBounds {
                offset: STRINGS as u64,
            },
        );
        let mut bytes = clean.clone();
        put_u64(
            &mut bytes,
            DYNAMIC + 2 * 16 + 8,
            (clean.len() - STRINGS + 1) as u64,
        );
        check(
            "dynamic string offset",
            bytes,
            Elf64Error::StringOffsetOutOfBounds {
                tag: DT_NEEDED,
                index: 2,
                offset: (clean.len() - STRINGS + 1) as u64,
            },
        );
        let mut bytes = clean.clone();
        let table_len = clean.len() - STRINGS;
        put_u64(&mut bytes, DYNAMIC + 2 * 16 + 8, (table_len - 1) as u64);
        bytes[clean.len() - 1] = b'x';
        check(
            "dynamic string terminator",
            bytes,
            Elf64Error::StringNotTerminated {
                tag: DT_NEEDED,
                index: 2,
                offset: (table_len - 1) as u64,
            },
        );
        let mut bytes = clean.clone();
        bytes[STRINGS + 1] = 0xff;
        check(
            "dynamic string UTF-8",
            bytes,
            Elf64Error::StringNotUtf8 {
                tag: DT_NEEDED,
                index: 2,
                offset: 1,
            },
        );
        let mut bytes = clean.clone();
        bytes.resize(STRINGS + MAX_STRING + 3, 0);
        bytes[STRINGS + 1..STRINGS + MAX_STRING + 2].fill(b'a');
        let total = bytes.len() as u64;
        put_u64(&mut bytes, PH + 32, total);
        put_u64(&mut bytes, DYNAMIC + 16 + 8, (MAX_STRING + 3) as u64);
        check(
            "dynamic string length",
            bytes,
            Elf64Error::StringTooLong {
                tag: DT_NEEDED,
                index: 2,
                offset: 1,
            },
        );

        let mut bytes = clean.clone();
        const LARGE_STRINGS: usize = 3000;
        bytes.resize(LARGE_STRINGS + 2, 0);
        let total = bytes.len() as u64;
        put_u64(&mut bytes, PH + 32, total);
        put_u64(&mut bytes, DYNAMIC + 8, BASE + LARGE_STRINGS as u64);
        put_u64(&mut bytes, DYNAMIC + 16 + 8, 2);
        for index in 0..=MAX_NEEDED {
            put_u64(&mut bytes, DYNAMIC + (index + 2) * 16, DT_NEEDED);
            put_u64(&mut bytes, DYNAMIC + (index + 2) * 16 + 8, 0);
        }
        put_u64(&mut bytes, DYNAMIC + (MAX_NEEDED + 3) * 16, DT_NULL);
        put_u64(&mut bytes, PH + 112 + 32, ((MAX_NEEDED + 4) * 16) as u64);
        check(
            "needed library count",
            bytes,
            Elf64Error::NeededCountExceeded {
                actual: MAX_NEEDED + 1,
                limit: MAX_NEEDED,
            },
        );
        let mut bytes = clean;
        put_u64(&mut bytes, DYNAMIC + 4 * 16, DT_SONAME);
        check(
            "duplicate singleton dynamic tag",
            bytes,
            Elf64Error::DuplicateSingletonTag {
                tag: DT_SONAME,
                index: 4,
            },
        );
    }

    #[test]
    fn parses_the_real_rustc_test_pie() {
        let bytes = fs::read(std::env::current_exe().unwrap()).unwrap();
        let parsed = parse_elf64(&bytes).unwrap();
        assert_eq!(parsed.elf_type, 3);
        assert_eq!(parsed.machine, 62);
        assert!(!parsed.interpreter.is_empty());
        assert!(!parsed.needed.is_empty());
    }

    #[test]
    fn derive_requested_release_binary() {
        let Ok(path) = std::env::var("SOLSTONE_ELF_DERIVE") else {
            return;
        };
        let parsed = parse_elf64(&fs::read(path).unwrap()).unwrap();
        eprintln!("DERIVED_ELF64={parsed:?}");
    }
}
