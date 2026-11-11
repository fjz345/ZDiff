//! A PDB as text for the text diff: one record per line, sorted, under `== Section ==` headers.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    io::Cursor,
};

use pdb::{
    ClassKind, FallibleIterator, HeaderVersion, IdData, IdFinder, IdIndex, PdbInternalSectionOffset,
    PrimitiveType, SymbolData, Type, TypeData, TypeFinder, TypeIndex,
};

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(default)
)]
pub struct PdbDumpOptions {
    pub public_only: bool,
    pub ignore_addresses: bool,
    pub ignore_identity: bool,
}

impl Default for PdbDumpOptions {
    fn default() -> Self {
        Self {
            public_only: false,
            ignore_addresses: false,
            ignore_identity: true,
        }
    }
}

pub const MAX_LINES: usize = 1_000_000;

const MSF_7_MAGIC: &[u8] = b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0";
const TPI_STREAM: pdb::StreamIndex = pdb::StreamIndex(2);
const DBI_STREAM: pdb::StreamIndex = pdb::StreamIndex(3);
// The DBI header's flags; the pdb crate doesn't expose fStripped (private data removed).
const DBI_FLAGS_OFFSET: usize = 56;
const DBI_FLAG_STRIPPED: u16 = 0x2;
// Only procedures are parsed from module streams; the _ID kinds' type index is into the IPI.
const PROC_KINDS: [u16; 5] = [0x100a, 0x100b, 0x110f, 0x1110, 0x1155];
const ID_PROC_KINDS: [u16; 3] = [0x1146, 0x1147, 0x1156];
const MAX_TYPE_DEPTH: u32 = 16;
const MAX_TYPE_NAME_NODES: u32 = 1024;
const MAX_FIELD_LISTS: usize = 4096;

pub fn is_msf(bytes: &[u8]) -> bool {
    bytes.starts_with(MSF_7_MAGIC)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdbModel {
    pub version: String,
    pub machine: String,
    pub guid: String,
    pub age: u32,
    pub timestamp: u32,
    pub stripped: bool,
    pub modules: Vec<String>,
    pub publics: Vec<Symbol>,
    pub globals: Vec<Symbol>,
    pub types: Vec<String>,
    pub undecoded: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Symbol {
    pub kind: &'static str,
    pub name: String,
    pub detail: String,
    pub address: String,
}

pub fn dump(bytes: &[u8], options: &PdbDumpOptions) -> Result<String, pdb::Error> {
    Ok(render(&extract(bytes)?, options))
}

pub fn extract(bytes: &[u8]) -> Result<PdbModel, pdb::Error> {
    let mut pdb = pdb::PDB::open(Cursor::new(bytes))?;
    let info = pdb.pdb_information()?;
    let dbi = pdb.debug_information()?;
    let flags = pdb.raw_stream(DBI_STREAM)?.and_then(|stream| {
        let bytes = stream.as_slice().get(DBI_FLAGS_OFFSET..DBI_FLAGS_OFFSET + 2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    });
    let mut model = PdbModel {
        version: match info.version {
            HeaderVersion::OtherValue(version) => version.to_string(),
            version => format!("{version:?}"),
        },
        machine: dbi.machine_type()?.to_string(),
        guid: info.guid.to_string().to_uppercase(),
        age: info.age,
        timestamp: info.signature,
        stripped: flags.is_some_and(|flags| flags & DBI_FLAG_STRIPPED != 0),
        ..Default::default()
    };
    let address_map = pdb.address_map().ok();
    let address = |offset: PdbInternalSectionOffset| {
        match address_map.as_ref().and_then(|map| offset.to_rva(map)) {
            Some(rva) => format!("0x{:08X}", rva.0),
            None => format!("{:04X}:{:08X}", offset.section, offset.offset),
        }
    };

    let type_info = pdb.type_information()?;
    let mut finder = type_info.finder();
    let mut iter = type_info.iter();
    while iter.next()?.is_some() {
        finder.update(&iter);
    }
    let types = Types {
        finder,
        panicking: match pdb.raw_stream(TPI_STREAM)? {
            Some(stream) => panicking_records(stream.as_slice()),
            None => HashSet::new(),
        },
        names: RefCell::default(),
    };
    let mut iter = type_info.iter();
    while let Some(item) = iter.next()? {
        match types.parse_item(&item) {
            Some(data) => model.types.extend(type_line(&types, &data)),
            None => model.undecoded += 1,
        }
    }
    // Older PDBs have no IPI stream; their procedures reference the TPI.
    let id_info = pdb.id_information().ok();
    let mut ids = id_info.as_ref().map(|info| info.finder());
    if let (Some(info), Some(ids)) = (&id_info, &mut ids) {
        let mut iter = info.iter();
        while iter.next()?.is_some() {
            ids.update(&iter);
        }
    }

    let globals = pdb.global_symbols()?;
    let mut iter = globals.iter();
    while let Some(symbol) = iter.next()? {
        match symbol.parse() {
            Ok(SymbolData::Public(public)) => model.publics.push(Symbol {
                kind: match (public.function, public.code) {
                    (true, _) => "function",
                    (false, true) => "code",
                    (false, false) => "data",
                },
                name: public.name.to_string().into_owned(),
                detail: String::new(),
                address: address(public.offset),
            }),
            Ok(data) => model.globals.extend(global_symbol(&types, &data, &address)),
            Err(_) => model.undecoded += 1,
        }
    }

    let mut modules = dbi.modules()?;
    while let Some(module) = modules.next()? {
        let (name, object) = (module.module_name(), module.object_file_name());
        model.modules.push(if object.is_empty() || name == object {
            name.into_owned()
        } else {
            format!("{name} (from {object})")
        });
        let Some(module_info) = pdb.module_info(&module)? else {
            continue;
        };
        let mut iter = module_info.symbols()?;
        while let Some(symbol) = iter.next()? {
            let by_id = ID_PROC_KINDS.contains(&symbol.raw_kind());
            if !by_id && !PROC_KINDS.contains(&symbol.raw_kind()) {
                continue;
            }
            match symbol.parse() {
                Ok(SymbolData::Procedure(proc)) => {
                    let signature = if by_id {
                        ids.as_ref()
                            .and_then(|ids| function_type(ids, IdIndex(proc.type_index.0)))
                            .map_or_else(|| "?".to_owned(), |index| type_name(&types, index))
                    } else {
                        type_name(&types, proc.type_index)
                    };
                    model.globals.push(Symbol {
                        kind: if proc.global { "proc" } else { "static proc" },
                        name: proc.name.to_string().into_owned(),
                        detail: format!(": {signature} size {}", proc.len),
                        address: address(proc.offset),
                    });
                }
                Ok(_) => {}
                Err(_) => model.undecoded += 1,
            }
        }
    }
    Ok(model)
}

struct Types<'t> {
    finder: TypeFinder<'t>,
    panicking: HashSet<TypeIndex>,
    names: RefCell<HashMap<TypeIndex, String>>,
}

impl<'t> Types<'t> {
    fn parse_item(&self, item: &Type<'t>) -> Option<TypeData<'t>> {
        if self.panicking.contains(&item.index()) {
            return None;
        }
        item.parse().ok()
    }

    fn parse(&self, index: TypeIndex) -> Option<TypeData<'t>> {
        self.parse_item(&self.finder.find(index).ok()?)
    }
}

// pdb 0.8 panics on some malformed records (assert!, unreachable!, unbounded field list
// recursion) instead of returning an error; release builds abort on a panic.
fn panicking_records(tpi: &[u8]) -> HashSet<TypeIndex> {
    let mut panicking = HashSet::new();
    let (Some(header_size), Some(mut index)) = (le_u32(tpi, 4), le_u32(tpi, 8)) else {
        return panicking;
    };
    let mut rest = tpi.get(header_size as usize..).unwrap_or_default();
    while let Some(len) = take_u16(&mut rest) {
        let Some(record) = take(&mut rest, len.into()) else {
            break;
        };
        if check_record(record).is_none() {
            panicking.insert(TypeIndex(index));
        }
        index += 1;
    }
    panicking
}

mod leaf {
    pub const POINTER: u16 = 0x1002;
    pub const ARRAY_ST: u16 = 0x1003;
    pub const ARRAY: u16 = 0x1503;
    pub const STRIDED_ARRAY: u16 = 0x1516;
    pub const FIELDLIST: u16 = 0x1203;
    pub const ST_MAX: u16 = 0x1500;
    pub const INDEX: u16 = 0x1404;
    pub const MEMBER: u16 = 0x150D;
    pub const MEMBER_ST: u16 = 0x1405;
    pub const STMEMBER: u16 = 0x150E;
    pub const STMEMBER_ST: u16 = 0x1406;
    pub const METHOD: u16 = 0x150F;
    pub const METHOD_ST: u16 = 0x1407;
    pub const NESTTYPE: u16 = 0x1510;
    pub const NESTTYPE_ST: u16 = 0x1408;
    pub const NESTTYPEEX: u16 = 0x1512;
    pub const NESTTYPEEX_ST: u16 = 0x140D;
    pub const ONEMETHOD: u16 = 0x1511;
    pub const ONEMETHOD_ST: u16 = 0x140B;
    pub const BCLASS: u16 = 0x1400;
    pub const BINTERFACE: u16 = 0x151A;
    pub const VBCLASS: u16 = 0x1401;
    pub const IVBCLASS: u16 = 0x1402;
    pub const VFUNCTAB: u16 = 0x1409;
    pub const ENUMERATE: u16 = 0x1502;
    pub const ENUMERATE_ST: u16 = 0x0403;
    pub const NUMERIC: u16 = 0x8000;
    pub const CHAR: u16 = 0x8000;
    pub const SHORT: u16 = 0x8001;
    pub const USHORT: u16 = 0x8002;
    pub const LONG: u16 = 0x8003;
    pub const ULONG: u16 = 0x8004;
    pub const QUADWORD: u16 = 0x8009;
    pub const UQUADWORD: u16 = 0x800A;
}

// None for any record whose parse could panic, and for malformed ones the parser rejects anyway.
fn check_record(mut record: &[u8]) -> Option<()> {
    let buf = &mut record;
    match take_u16(buf)? {
        leaf::POINTER => {
            take(buf, 4)?;
            let mode = (le_u32(take(buf, 4)?, 0)? >> 5) & 0x7;
            (mode <= 4).then_some(())
        }
        kind @ (leaf::ARRAY | leaf::ARRAY_ST | leaf::STRIDED_ARRAY) => {
            take(buf, if kind == leaf::STRIDED_ARRAY { 12 } else { 8 })?;
            loop {
                numeric(buf, false)?;
                if *buf.first()? == 0 {
                    take(buf, 1)?;
                    break;
                }
            }
            padding(buf)?;
            buf.is_empty().then_some(())
        }
        leaf::FIELDLIST => {
            while !buf.is_empty() {
                field(buf)?;
                padding(buf)?;
            }
            Some(())
        }
        _ => Some(()),
    }
}

// Mirrors pdb's own field grammar, so record boundaries match its parse; any other kind there
// is a type record, which a real field list never nests.
fn field(buf: &mut &[u8]) -> Option<()> {
    use leaf::*;
    let kind = take_u16(buf)?;
    match kind {
        INDEX => take(buf, 4).map(drop),
        MEMBER | MEMBER_ST => {
            take(buf, 6)?;
            numeric(buf, false)?;
            string(buf, kind)
        }
        STMEMBER | STMEMBER_ST | METHOD | METHOD_ST | NESTTYPE | NESTTYPE_ST | NESTTYPEEX
        | NESTTYPEEX_ST => {
            take(buf, 6)?;
            string(buf, kind)
        }
        ONEMETHOD | ONEMETHOD_ST => {
            let intro_virtual = matches!((take_u16(buf)? >> 2) & 0x7, 4 | 6);
            take(buf, if intro_virtual { 8 } else { 4 })?;
            string(buf, kind)
        }
        BCLASS | BINTERFACE => {
            take(buf, 6)?;
            numeric(buf, false)
        }
        VBCLASS | IVBCLASS => {
            take(buf, 10)?;
            numeric(buf, false)?;
            numeric(buf, false)
        }
        VFUNCTAB => take(buf, 6).map(drop),
        ENUMERATE | ENUMERATE_ST => {
            take(buf, 2)?;
            numeric(buf, true)?;
            string(buf, kind)
        }
        _ => None,
    }
}

fn numeric(buf: &mut &[u8], signed: bool) -> Option<()> {
    use leaf::*;
    let size = match take_u16(buf)? {
        ..NUMERIC => 0,
        CHAR => 1,
        USHORT => 2,
        ULONG => 4,
        UQUADWORD => 8,
        SHORT if signed => 2,
        LONG if signed => 4,
        QUADWORD if signed => 8,
        _ => return None,
    };
    take(buf, size).map(drop)
}

fn string(buf: &mut &[u8], kind: u16) -> Option<()> {
    let len = if kind > leaf::ST_MAX {
        buf.iter().position(|&byte| byte == 0)? + 1
    } else {
        usize::from(*take(buf, 1)?.first()?)
    };
    take(buf, len).map(drop)
}

fn padding(buf: &mut &[u8]) -> Option<()> {
    while let Some(&pad @ 0xF0..) = buf.first() {
        take(buf, 1)?;
        if pad > 0xF0 {
            take(buf, usize::from(pad & 0x0F) - 1)?;
        }
    }
    Some(())
}

fn take<'b>(buf: &mut &'b [u8], n: usize) -> Option<&'b [u8]> {
    let (head, tail) = buf.split_at_checked(n)?;
    *buf = tail;
    Some(head)
}

fn take_u16(buf: &mut &[u8]) -> Option<u16> {
    let bytes = take(buf, 2)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let bytes = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn global_symbol(
    types: &Types,
    data: &SymbolData,
    address: &impl Fn(PdbInternalSectionOffset) -> String,
) -> Option<Symbol> {
    let (kind, name, detail, address) = match data {
        SymbolData::Data(data) => (
            if data.global { "data" } else { "static data" },
            data.name,
            format!(": {}", type_name(types, data.type_index)),
            address(data.offset),
        ),
        SymbolData::ThreadStorage(tls) => (
            if tls.global { "tls" } else { "static tls" },
            tls.name,
            format!(": {}", type_name(types, tls.type_index)),
            address(tls.offset),
        ),
        SymbolData::UserDefinedType(udt) => (
            "typedef",
            udt.name,
            format!(": {}", type_name(types, udt.type_index)),
            String::new(),
        ),
        SymbolData::Constant(constant) => (
            "const",
            constant.name,
            format!(
                ": {} = {}",
                type_name(types, constant.type_index),
                constant.value
            ),
            String::new(),
        ),
        _ => return None,
    };
    Some(Symbol {
        kind,
        name: name.to_string().into_owned(),
        detail,
        address,
    })
}

fn function_type(ids: &IdFinder, index: IdIndex) -> Option<TypeIndex> {
    match ids.find(index).and_then(|item| item.parse()).ok()? {
        IdData::Function(function) => Some(function.function_type),
        IdData::MemberFunction(function) => Some(function.function_type),
        _ => None,
    }
}

fn type_line(types: &Types, data: &TypeData) -> Option<String> {
    let (kind, name, size, field_list) = match data {
        TypeData::Class(class) if !class.properties.forward_reference() => (
            match class.kind {
                ClassKind::Class => "class",
                ClassKind::Struct => "struct",
                ClassKind::Interface => "interface",
            },
            class.name,
            class.size,
            class.fields,
        ),
        TypeData::Union(union) if !union.properties.forward_reference() => {
            ("union", union.name, union.size, Some(union.fields))
        }
        TypeData::Enumeration(enumeration) if !enumeration.properties.forward_reference() => {
            let members = fields(types, enumeration.fields).join(", ");
            let underlying = type_name(types, enumeration.underlying_type);
            return Some(format!(
                "enum {}: {underlying} {{ {members} }}",
                enumeration.name
            ));
        }
        _ => return None,
    };
    let members = field_list.map_or_else(String::new, |index| fields(types, index).join("; "));
    Some(format!("{kind} {name} size {size} {{ {members} }}"))
}

fn fields(types: &Types, index: TypeIndex) -> Vec<String> {
    let mut out = Vec::new();
    let mut next = Some(index);
    for _ in 0..MAX_FIELD_LISTS {
        let Some(index) = next.take() else {
            break;
        };
        let Some(TypeData::FieldList(list)) = types.parse(index) else {
            out.push("?".to_owned());
            break;
        };
        for field in &list.fields {
            out.extend(match field {
                TypeData::Member(member) => Some(format!(
                    "{} {}@{}",
                    type_name(types, member.field_type),
                    member.name,
                    member.offset
                )),
                TypeData::BaseClass(base) => Some(format!(
                    "base {}@{}",
                    type_name(types, base.base_class),
                    base.offset
                )),
                TypeData::VirtualBaseClass(base) => {
                    Some(format!("virtual base {}", type_name(types, base.base_class)))
                }
                TypeData::StaticMember(member) => Some(format!(
                    "static {} {}",
                    type_name(types, member.field_type),
                    member.name
                )),
                TypeData::Enumerate(value) => Some(format!("{} = {}", value.name, value.value)),
                _ => None,
            });
        }
        next = list.continuation;
    }
    out
}

fn type_name(types: &Types, index: TypeIndex) -> String {
    if let Some(name) = types.names.borrow().get(&index) {
        return name.clone();
    }
    let mut nodes_left = MAX_TYPE_NAME_NODES;
    let name = type_name_at(types, index, 0, &mut nodes_left);
    types.names.borrow_mut().insert(index, name.clone());
    name
}

// Argument lists fan out at every level, so the depth cap alone still allows args^depth nodes.
fn type_name_at(types: &Types, index: TypeIndex, depth: u32, nodes_left: &mut u32) -> String {
    if depth > MAX_TYPE_DEPTH || *nodes_left == 0 {
        return "...".to_owned();
    }
    *nodes_left -= 1;
    let mut inner = |index| type_name_at(types, index, depth + 1, nodes_left);
    let Some(data) = types.parse(index) else {
        return "?".to_owned();
    };
    match data {
        TypeData::Primitive(primitive) => primitive_name(primitive),
        TypeData::Class(class) => class.name.to_string().into_owned(),
        TypeData::Union(union) => union.name.to_string().into_owned(),
        TypeData::Enumeration(enumeration) => enumeration.name.to_string().into_owned(),
        TypeData::Pointer(pointer) => {
            let sigil = if pointer.attributes.is_reference() { "&" } else { "*" };
            format!("{}{sigil}", inner(pointer.underlying_type))
        }
        TypeData::Modifier(modifier) => {
            let mut name = String::new();
            if modifier.constant {
                name.push_str("const ");
            }
            if modifier.volatile {
                name.push_str("volatile ");
            }
            name + &inner(modifier.underlying_type)
        }
        TypeData::Array(array) => {
            let bytes: Vec<String> = array.dimensions.iter().map(u32::to_string).collect();
            format!("{}[{}B]", inner(array.element_type), bytes.join("x"))
        }
        TypeData::Bitfield(bitfield) => {
            format!("{}:{}", inner(bitfield.underlying_type), bitfield.length)
        }
        TypeData::Procedure(procedure) => signature(
            types,
            procedure.return_type,
            procedure.argument_list,
            depth,
            nodes_left,
        ),
        TypeData::MemberFunction(function) => signature(
            types,
            Some(function.return_type),
            function.argument_list,
            depth,
            nodes_left,
        ),
        _ => "?".to_owned(),
    }
}

fn signature(
    types: &Types,
    return_type: Option<TypeIndex>,
    arguments: TypeIndex,
    depth: u32,
    nodes_left: &mut u32,
) -> String {
    let mut inner = |index| type_name_at(types, index, depth + 1, nodes_left);
    let arguments = match types.parse(arguments) {
        Some(TypeData::ArgumentList(list)) => {
            let names: Vec<String> = list.arguments.iter().map(|&index| inner(index)).collect();
            names.join(", ")
        }
        _ => "?".to_owned(),
    };
    let return_type = return_type.map_or_else(|| "void".to_owned(), inner);
    format!("fn({arguments}) -> {return_type}")
}

fn primitive_name(primitive: PrimitiveType) -> String {
    let name = format!("{:?}", primitive.kind).to_lowercase();
    match primitive.indirection {
        Some(_) => name + "*",
        None => name,
    }
}

pub fn render(model: &PdbModel, options: &PdbDumpOptions) -> String {
    render_capped(model, options, MAX_LINES)
}

fn render_capped(model: &PdbModel, options: &PdbDumpOptions, max_lines: usize) -> String {
    let mut out = Lines {
        text: String::new(),
        written: 0,
        dropped: 0,
        dropped_hash: DefaultHasher::new(),
        max_lines,
    };
    out.push("== Header ==");
    out.push(&format!("Version: {}", model.version));
    out.push(&format!("Machine: {}", model.machine));
    out.push(if model.stripped {
        "Stripped: yes (private symbols and types removed)"
    } else {
        "Stripped: no"
    });
    if !options.ignore_identity {
        out.push(&format!("GUID: {}", model.guid));
        out.push(&format!("Age: {}", model.age));
        out.push(&format!("Timestamp: 0x{:08X}", model.timestamp));
    }
    if !options.public_only && model.undecoded > 0 {
        out.push(&format!("Undecoded records: {}", model.undecoded));
    }

    out.section("Modules", sorted(&model.modules));
    out.symbols("Public symbols", &model.publics, options);
    if !options.public_only {
        out.symbols("Global symbols", &model.globals, options);
        out.section("Types", sorted(&model.types));
    }
    if out.dropped > 0 {
        // The hash makes two dumps that differ only in the dropped lines still differ.
        let marker = format!(
            "== Truncated: {} more lines not shown (limit {max_lines}, hash {:016x}) ==",
            out.dropped,
            out.dropped_hash.finish()
        );
        out.text.push_str(&marker);
        out.text.push('\n');
    }
    out.text
}

fn sorted(lines: &[String]) -> Vec<&str> {
    let mut lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    lines.sort_unstable();
    lines
}

struct Lines {
    text: String,
    written: usize,
    dropped: usize,
    dropped_hash: DefaultHasher,
    max_lines: usize,
}

impl Lines {
    fn push(&mut self, line: &str) {
        if self.written == self.max_lines {
            self.dropped += 1;
            line.hash(&mut self.dropped_hash);
            return;
        }
        self.written += 1;
        self.text.push_str(line);
        self.text.push('\n');
    }

    fn section(&mut self, title: &str, lines: Vec<&str>) {
        self.push("");
        self.push(&format!("== {title} ({}) ==", lines.len()));
        for line in lines {
            self.push(line);
        }
    }

    fn symbols(&mut self, title: &str, symbols: &[Symbol], options: &PdbDumpOptions) {
        let mut symbols: Vec<&Symbol> = symbols.iter().collect();
        symbols.sort_unstable();
        self.push("");
        self.push(&format!("== {title} ({}) ==", symbols.len()));
        for symbol in symbols {
            let mut line = format!("{} {}{}", symbol.kind, symbol.name, symbol.detail);
            if !options.ignore_addresses && !symbol.address.is_empty() {
                line.push_str(" @");
                line.push_str(&symbol.address);
            }
            self.push(&line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(kind: &'static str, name: &str, detail: &str, address: &str) -> Symbol {
        Symbol {
            kind,
            name: name.to_owned(),
            detail: detail.to_owned(),
            address: address.to_owned(),
        }
    }

    fn full() -> PdbModel {
        PdbModel {
            version: "V70".into(),
            machine: "Amd64".into(),
            guid: "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0".into(),
            age: 3,
            timestamp: 0x5F00_1234,
            stripped: false,
            modules: vec!["b.obj".into(), "a.obj (from x.lib)".into()],
            publics: vec![
                symbol("function", "?zeta@@YAXXZ", "", "0x00002000"),
                symbol("data", "?alpha@@3HA", "", "0x00005000"),
                symbol("function", "?beta@@YAXXZ", "", "0x00001000"),
            ],
            globals: vec![
                symbol("proc", "zeta", ": fn() -> void size 16", "0x00002000"),
                symbol("data", "alpha", ": i32", "0x00005000"),
                symbol("typedef", "Alpha", ": Alpha", ""),
            ],
            types: vec![
                "struct Zed size 8 { i32 a@0; i32 b@4 }".into(),
                "struct Alpha size 4 { i32 x@0 }".into(),
            ],
            undecoded: 2,
        }
    }

    fn reversed(mut model: PdbModel) -> PdbModel {
        model.modules.reverse();
        model.publics.reverse();
        model.globals.reverse();
        model.types.reverse();
        model
    }

    fn section<'a>(text: &'a str, title: &str) -> Vec<&'a str> {
        let header = format!("== {title} (");
        let mut lines = text.lines().skip_while(|line| !line.starts_with(&header));
        assert!(lines.next().is_some(), "no {title} section in\n{text}");
        lines.take_while(|line| !line.is_empty()).collect()
    }

    #[test]
    fn every_section_is_sorted_so_record_order_does_not_matter() {
        let options = PdbDumpOptions::default();
        let text = render(&full(), &options);

        assert_eq!(text, render(&reversed(full()), &options));
        assert_eq!(section(&text, "Modules"), ["a.obj (from x.lib)", "b.obj"]);
        assert_eq!(
            section(&text, "Public symbols"),
            [
                "data ?alpha@@3HA @0x00005000",
                "function ?beta@@YAXXZ @0x00001000",
                "function ?zeta@@YAXXZ @0x00002000",
            ]
        );
        assert_eq!(
            section(&text, "Global symbols"),
            [
                "data alpha: i32 @0x00005000",
                "proc zeta: fn() -> void size 16 @0x00002000",
                "typedef Alpha: Alpha",
            ]
        );
        assert_eq!(
            section(&text, "Types"),
            [
                "struct Alpha size 4 { i32 x@0 }",
                "struct Zed size 8 { i32 a@0; i32 b@4 }",
            ]
        );
    }

    #[test]
    fn identity_is_hidden_by_default_so_a_rebuild_compares_clean() {
        let rebuilt = PdbModel {
            guid: "11111111-2222-3333-4444-555555555555".into(),
            age: 1,
            timestamp: 0x6000_0000,
            ..full()
        };
        let options = PdbDumpOptions::default();
        assert_eq!(render(&full(), &options), render(&rebuilt, &options));

        let shown = PdbDumpOptions {
            ignore_identity: false,
            ..options
        };
        let text = render(&full(), &shown);
        assert!(text.contains("GUID: 0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0\n"), "{text}");
        assert!(text.contains("Age: 3\n"), "{text}");
        assert!(text.contains("Timestamp: 0x5F001234\n"), "{text}");
        assert_ne!(text, render(&rebuilt, &shown));
    }

    #[test]
    fn ignoring_addresses_drops_them_from_every_symbol() {
        let mut moved = full();
        for symbol in moved.publics.iter_mut().chain(&mut moved.globals) {
            symbol.address.push('0');
        }
        let options = PdbDumpOptions {
            ignore_addresses: true,
            ..Default::default()
        };

        let text = render(&full(), &options);

        assert_eq!(text, render(&moved, &options));
        assert!(!text.contains(" @"), "{text}");
        assert_eq!(
            section(&text, "Global symbols"),
            [
                "data alpha: i32",
                "proc zeta: fn() -> void size 16",
                "typedef Alpha: Alpha",
            ]
        );
    }

    #[test]
    fn public_only_compares_a_full_pdb_clean_against_its_stripped_copy() {
        let stripped = PdbModel {
            stripped: true,
            globals: Vec::new(),
            types: Vec::new(),
            undecoded: 0,
            ..full()
        };
        let options = PdbDumpOptions {
            public_only: true,
            ..Default::default()
        };

        let full_text = render(&full(), &options);
        let stripped_text = render(&stripped, &options);

        let differing: Vec<_> = full_text
            .lines()
            .zip(stripped_text.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(
            differing,
            [("Stripped: no", "Stripped: yes (private symbols and types removed)")]
        );
        assert_eq!(full_text.lines().count(), stripped_text.lines().count());
        assert!(!full_text.contains("== Global symbols"), "{full_text}");
        assert!(!full_text.contains("== Types"), "{full_text}");
        let all = render(&full(), &PdbDumpOptions::default());
        assert!(all.contains("== Types (2) =="), "{all}");
        assert!(all.contains("Undecoded records: 2\n"), "{all}");
    }

    #[test]
    fn a_dump_over_the_line_limit_ends_with_a_truncation_marker() {
        let options = PdbDumpOptions::default();
        let uncapped = render(&full(), &options);
        let total = uncapped.lines().count();

        let text = render_capped(&full(), &options, 5);

        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines[..5], uncapped.lines().take(5).collect::<Vec<_>>()[..]);
        assert_eq!(lines.len(), 6, "{text}");
        let marker = format!(
            "== Truncated: {} more lines not shown (limit 5, hash ",
            total - 5
        );
        assert!(lines[5].starts_with(&marker), "{text}");
        assert_eq!(render_capped(&full(), &options, total), uncapped);
    }

    #[test]
    fn dumps_that_differ_only_past_the_line_limit_still_differ() {
        let options = PdbDumpOptions::default();
        let mut changed = full();
        changed.types[0] = "struct Zed size 8 { i32 a@0; i32 c@4 }".into();
        let total = render(&full(), &options).lines().count();
        let cap = total - 2;
        assert_eq!(
            render_capped(&full(), &options, cap).lines().count(),
            render_capped(&changed, &options, cap).lines().count()
        );
        assert_ne!(
            render_capped(&full(), &options, cap),
            render_capped(&changed, &options, cap)
        );
    }

    #[test]
    fn only_msf_7_bytes_are_parsed_and_a_broken_one_is_an_error() {
        assert!(!is_msf(b"HEADER    PROTEIN DATA BANK\n"));
        assert!(!is_msf(b"Microsoft C/C++ program database 2.00\r\n\x1aJG\0\0"));
        let truncated = [MSF_7_MAGIC, &[0u8; 8][..]].concat();
        assert!(is_msf(&truncated));
        assert!(extract(&truncated).is_err());
    }

    #[cfg(all(windows, target_env = "msvc"))]
    #[test]
    fn the_test_binary_pdb_lists_its_own_functions_and_types() {
        let path = std::env::current_exe().unwrap().with_extension("pdb");
        let bytes = std::fs::read(&path).unwrap();
        assert!(is_msf(&bytes), "{}", path.display());

        let model = extract(&bytes).unwrap();

        assert!(!model.stripped);
        assert!(!model.modules.is_empty() && !model.publics.is_empty());
        let extract_proc = model
            .globals
            .iter()
            .find(|s| s.kind == "proc" && s.name == "zdiff_gui::pdb_dump::extract")
            .expect("no proc for pdb_dump::extract");
        assert!(extract_proc.detail.starts_with(": fn("), "{extract_proc:?}");
        let symbol_type = model
            .types
            .iter()
            .find(|t| t.starts_with("struct zdiff_gui::pdb_dump::Symbol "))
            .expect("no type line for pdb_dump::Symbol");
        for field in [" kind@", " name@", " detail@", " address@"] {
            assert!(symbol_type.contains(field), "{symbol_type}");
        }
    }

    #[cfg(all(windows, target_env = "msvc"))]
    fn u32_at(bytes: &[u8], at: usize) -> usize {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
    }

    #[cfg(all(windows, target_env = "msvc"))]
    fn record(leaf: u16, parts: &[&[u8]]) -> Vec<u8> {
        let leaf = leaf.to_le_bytes();
        [&leaf[..]].iter().chain(parts).copied().flatten().copied().collect()
    }

    // Rewrites the TPI stream in its own blocks; LF_MODIFIER fillers keep its size, so the
    // stream directory stays valid.
    #[cfg(all(windows, target_env = "msvc"))]
    fn own_pdb_with_types(records: &[Vec<u8>]) -> Vec<u8> {
        let path = std::env::current_exe().unwrap().with_extension("pdb");
        let mut pdb = std::fs::read(&path).unwrap();
        let block = u32_at(&pdb, 32);
        let block_map = u32_at(&pdb, 52) * block;
        let directory: Vec<u8> = (0..u32_at(&pdb, 44).div_ceil(block))
            .flat_map(|i| {
                let at = u32_at(&pdb, block_map + 4 * i) * block;
                pdb[at..at + block].to_vec()
            })
            .collect();
        let stream_count = u32_at(&directory, 0);
        let size = |stream: usize| match u32_at(&directory, 4 + 4 * stream) {
            0xFFFF_FFFF => 0,
            size => size,
        };
        let first_block: usize = (0..2).map(|stream| size(stream).div_ceil(block)).sum();
        let blocks: Vec<usize> = (0..size(2).div_ceil(block))
            .map(|i| u32_at(&directory, 4 + 4 * (stream_count + first_block + i)))
            .collect();
        let header_size = u32_at(&pdb, blocks[0] * block + 4);

        let mut tpi = pdb[blocks[0] * block..][..header_size].to_vec();
        let mut count = 0;
        let mut push = |tpi: &mut Vec<u8>, record: &[u8]| {
            tpi.extend_from_slice(&u16::try_from(record.len()).unwrap().to_le_bytes());
            tpi.extend_from_slice(record);
            count += 1;
        };
        for record in records {
            push(&mut tpi, record);
        }
        while tpi.len() < size(2) {
            let remaining = size(2) - tpi.len() - 2;
            let len = remaining.min(0xFFFF);
            let len = if (1..10).contains(&(remaining - len)) { len - 10 } else { len };
            let mut filler = record(0x1001, &[&0x74u32.to_le_bytes()]);
            filler.resize(len, 0);
            push(&mut tpi, &filler);
        }
        assert_eq!(tpi.len(), size(2));
        let maximum = u32::try_from(u32_at(&tpi, 8) + count).unwrap();
        tpi[12..16].copy_from_slice(&maximum.to_le_bytes());
        for (chunk, b) in tpi.chunks(block).zip(&blocks) {
            pdb[b * block..][..chunk.len()].copy_from_slice(chunk);
        }
        pdb
    }

    #[cfg(all(windows, target_env = "msvc"))]
    #[test]
    fn records_that_would_panic_the_pdb_parser_are_counted_undecoded() {
        let i32 = 0x74u32.to_le_bytes();
        let ptr64 = 0x0001_000Cu32.to_le_bytes();
        let bad_mode = (0x0001_000Cu32 | 5 << 5).to_le_bytes();
        let array_head = [0x1000u32.to_le_bytes(), 0x23u32.to_le_bytes()].concat();
        let member = |kind: u32, offset: u8, name: &[u8]| {
            record(0x150D, &[&[3, 0], &kind.to_le_bytes(), &[offset, 0], name])
        };
        let members = [member(0x1001, 0, b"a\0\xF1"), member(0x1000, 16, b"b\0\xF1")].concat();
        let nested_lists = 0x1203u16.to_le_bytes().repeat(0x7FF0);
        let records = [
            record(0x1002, &[&i32, &ptr64]),
            record(0x1503, &[&array_head, &[16, 0, 0, 0xF3, 0xF2, 0xF1]]),
            record(0x1203, &[&members]),
            record(0x1505, &[&[2, 0, 0, 0], &0x1002u32.to_le_bytes(), &[0; 8], &[24, 0], b"Good\0"]),
            record(0x1002, &[&i32, &bad_mode]),
            record(0x1503, &[&array_head, &[16, 0, 0, b'x']]),
            record(0x1203, &[&0x1002u16.to_le_bytes(), &i32, &bad_mode]),
            record(0x1203, &[&nested_lists]),
            record(0x1505, &[&[1, 0, 0, 0], &0x1006u32.to_le_bytes(), &[0; 8], &[4, 0], b"Bad\0"]),
        ];
        let baseline = extract(&own_pdb_with_types(&[])).unwrap();

        let model = extract(&own_pdb_with_types(&records)).unwrap();

        assert_eq!(model.undecoded, baseline.undecoded + 4);
        let good = "struct Good size 24 { i32*[16B] a@0; i32* b@16 }";
        assert!(model.types.iter().any(|line| line == good), "{:?}", model.types);
        let bad = "struct Bad size 4 { ? }";
        assert!(model.types.iter().any(|line| line == bad), "{:?}", model.types);
    }

    #[cfg(all(windows, target_env = "msvc"))]
    #[test]
    fn a_type_name_fanning_out_at_every_level_is_cut_short() {
        let procedure = |arguments: u32| {
            record(0x1008, &[&0x03u32.to_le_bytes(), &[0, 0, 4, 0], &arguments.to_le_bytes()])
        };
        let mut records = vec![record(0x1201, &[&[0; 4]]), procedure(0x1000)];
        for level in 1..20u32 {
            let callee = 0x1000 + 2 * level - 1;
            records.push(record(0x1201, &[&4u32.to_le_bytes(), &callee.to_le_bytes().repeat(4)]));
            records.push(procedure(callee + 1));
        }
        let top = 0x1000 + records.len() as u32 - 1;
        let member = record(0x150D, &[&[3, 0], &top.to_le_bytes(), &[0, 0], b"f\0"]);
        records.push(record(0x1203, &[&member]));
        let list = 0x1000 + records.len() as u32 - 1;
        records.push(record(0x1505, &[&[1, 0, 0, 0], &list.to_le_bytes(), &[0; 8], &[8, 0], b"Fan\0"]));

        let model = extract(&own_pdb_with_types(&records)).unwrap();

        let fan = model.types.iter().find(|line| line.starts_with("struct Fan ")).unwrap();
        assert!(fan.starts_with("struct Fan size 8 { fn(fn(fn(fn("), "{fan}");
        assert!(fan.contains("..."), "{fan}");
        assert!(fan.len() < 64 * 1024, "{} bytes", fan.len());
    }
}
