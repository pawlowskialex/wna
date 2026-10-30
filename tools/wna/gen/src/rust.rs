//! Rust bindings with the same shapes as `wit-bindgen` (module paths, trait
//! names, parameter forms), so one implementation builds natively against
//! WNA tables or as a Wasm component with wit-bindgen.

use anyhow::{Result, bail};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;
use wna_abi::wit_parser::*;
use wna_abi::*;

pub struct Opts {
    /// Component name, e.g. `acme:core-rs`. Its symbol is
    /// `<name with [:-] → _>__wna_component`.
    pub name: String,
    /// Extra derives for records, variants (wit-bindgen's `additional_derives`).
    pub additional_derives: Vec<String>,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Dir {
    Import,
    Export,
}

pub fn symbol(name: &str) -> String {
    format!("{}__wna_component", name.replace([':', '-', '/', '.', '@'], "_"))
}

pub fn generate(wit: &Wit, world: WorldId, opts: &Opts) -> Result<String> {
    let (imports, exports) = wit.world_interfaces(world)?;
    for &i in imports.iter().chain(&exports) {
        wit.check_v0(i)?;
    }
    let mut g = Gen { wit, opts, imports: imports.clone(), exports: exports.clone(), dirs: HashMap::new(), modules: BTreeMap::new(), err_types: HashSet::new() };
    for &i in &exports {
        g.dirs.insert(i, Dir::Export);
    }
    // Every interface reachable through `use` gets a module; non-exported
    // ones live in the import tree (types-only if not imported).
    let mut todo: Vec<_> = imports.iter().chain(&exports).copied().collect();
    let mut all = Vec::new();
    while let Some(i) = todo.pop() {
        if all.contains(&i) {
            continue;
        }
        all.push(i);
        g.dirs.entry(i).or_insert(Dir::Import);
        todo.extend(used_interfaces(wit, i));
    }
    for &i in &all {
        for f in wit.resolve.interfaces[i].functions.values() {
            if let Some(Type::Id(r)) = f.result {
                if let TypeDefKind::Result(Result_ { err: Some(Type::Id(e)), .. }) = &wit.resolve.types[r].kind {
                    g.err_types.insert(wit.dealias_id(*e));
                }
            }
        }
    }
    all.sort_by_key(|i| (g.dirs[i] == Dir::Export, wit.iface_name(*i)));
    for &i in &all {
        let body = g.interface(i)?;
        let path = g.module_path(i);
        g.modules.insert(path, body);
    }
    let mut out = String::new();
    render_tree(&g.modules, &mut out);
    out.push_str(&g.world_module(world));
    Ok(out)
}

struct Gen<'a> {
    wit: &'a Wit,
    opts: &'a Opts,
    imports: Vec<InterfaceId>,
    exports: Vec<InterfaceId>,
    dirs: HashMap<InterfaceId, Dir>,
    modules: BTreeMap<Vec<String>, String>,
    err_types: HashSet<TypeId>,
}

/// Renders `{["exports","acme","core","storage"]: body}` as nested modules.
fn render_tree(modules: &BTreeMap<Vec<String>, String>, out: &mut String) {
    #[derive(Default)]
    struct Node {
        children: BTreeMap<String, Node>,
        body: Option<String>,
    }
    let mut root = Node::default();
    for (path, body) in modules {
        let mut n = &mut root;
        for seg in path {
            n = n.children.entry(seg.clone()).or_default();
        }
        n.body = Some(body.clone());
    }
    fn walk(n: &Node, depth: usize, out: &mut String) {
        for (name, c) in &n.children {
            if depth == 0 {
                out.push_str("#[allow(dead_code, unused_imports, unused_mut, unused_unsafe, non_snake_case, clippy::all)]\n");
            }
            writeln!(out, "pub mod {name} {{").unwrap();
            if let Some(b) = &c.body {
                out.push_str(b);
            }
            walk(c, depth + 1, out);
            out.push_str("}\n");
        }
    }
    walk(&root, 0, out);
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let",
    "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
    "true", "type", "unsafe", "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final",
    "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

fn rs_ident(s: &str) -> String {
    let n = snake(s);
    if RUST_KEYWORDS.contains(&n.as_str()) { format!("{n}_") } else { n }
}

fn rs_type_name(s: &str) -> String {
    upper_camel(s)
}

fn docs(d: &Docs, indent: &str) -> String {
    let mut s = String::new();
    if let Some(c) = &d.contents {
        for line in c.lines() {
            writeln!(s, "{indent}/// {line}").unwrap();
        }
    }
    s
}

const RT: &str = "::wna";

impl<'a> Gen<'a> {
    fn module_path(&self, iface: InterfaceId) -> Vec<String> {
        let i = &self.wit.resolve.interfaces[iface];
        let p = &self.wit.resolve.packages[i.package.unwrap()].name;
        let mut v = Vec::new();
        if self.dirs[&iface] == Dir::Export {
            v.push("exports".to_string());
        }
        v.push(rs_ident(&p.namespace));
        v.push(rs_ident(&p.name));
        v.push(rs_ident(i.name.as_ref().unwrap()));
        v
    }

    fn root_from(&self, iface: InterfaceId) -> String {
        "super::".repeat(self.module_path(iface).len())
    }

    fn world_path(&self, from: InterfaceId) -> String {
        format!("{}__wna_world", self.root_from(from))
    }

    /// Path to a named type from inside `from`'s module.
    fn named_path(&self, from: InterfaceId, id: TypeId) -> String {
        let t = &self.wit.resolve.types[id];
        let name = rs_type_name(t.name.as_ref().unwrap());
        match owner_iface(self.wit, id) {
            Some(o) if o == from => name,
            Some(o) => format!("{}{}::{name}", self.root_from(from), self.module_path(o).join("::")),
            None => name,
        }
    }

    fn resource_dir(&self, r: TypeId) -> Dir {
        let r = self.wit.dealias_id(r);
        self.dirs[&owner_iface(self.wit, r).unwrap()]
    }

    /// Owned form: results, export parameters, fields, list elements.
    fn owned(&self, from: InterfaceId, ty: Type) -> String {
        match ty {
            Type::Bool => "bool".into(),
            Type::U8 => "u8".into(),
            Type::U16 => "u16".into(),
            Type::U32 => "u32".into(),
            Type::U64 => "u64".into(),
            Type::S8 => "i8".into(),
            Type::S16 => "i16".into(),
            Type::S32 => "i32".into(),
            Type::S64 => "i64".into(),
            Type::F32 => "f32".into(),
            Type::F64 => "f64".into(),
            Type::Char => "char".into(),
            Type::String => "::std::string::String".into(),
            Type::ErrorContext => unreachable!(),
            Type::Id(id) => {
                let t = &self.wit.resolve.types[id];
                if t.name.is_some() {
                    return self.named_path(from, id);
                }
                match &t.kind {
                    TypeDefKind::List(e) => format!("::std::vec::Vec<{}>", self.owned(from, *e)),
                    TypeDefKind::Option(e) => format!("::core::option::Option<{}>", self.owned(from, *e)),
                    TypeDefKind::Result(r) => format!("::core::result::Result<{}, {}>", self.owned_opt(from, r.ok), self.owned_opt(from, r.err)),
                    TypeDefKind::Tuple(tu) => tuple(tu.types.iter().map(|t| self.owned(from, *t))),
                    TypeDefKind::Handle(Handle::Own(r)) => self.named_path(from, *r),
                    TypeDefKind::Handle(Handle::Borrow(r)) => {
                        let p = self.named_path(from, *r);
                        if self.resource_dir(*r) == Dir::Export { format!("{p}Borrow<'_>") } else { format!("&{p}") }
                    }
                    TypeDefKind::Type(t) => self.owned(from, *t),
                    k => panic!("unsupported {k:?}"),
                }
            }
        }
    }

    fn owned_opt(&self, from: InterfaceId, ty: Option<Type>) -> String {
        ty.map(|t| self.owned(from, t)).unwrap_or_else(|| "()".into())
    }

    /// Borrowed form for import parameters (wit-bindgen's default ownership).
    fn param(&self, from: InterfaceId, ty: Type) -> String {
        match ty {
            Type::String => "&str".into(),
            Type::Id(id) => {
                let t = &self.wit.resolve.types[id];
                if t.name.is_some() {
                    if let TypeDefKind::Type(inner) = t.kind {
                        if matches!(self.wit.dealias(inner), Type::Id(r) if matches!(self.wit.resolve.types[r].kind, TypeDefKind::Resource)) {
                            return self.named_path(from, id);
                        }
                    }
                    let p = self.named_path(from, id);
                    return if self.is_copy(ty) { p } else { format!("&{p}") };
                }
                match &t.kind {
                    TypeDefKind::List(e) => format!("&[{}]", self.owned(from, *e)),
                    TypeDefKind::Option(e) => format!("::core::option::Option<{}>", self.param(from, *e)),
                    TypeDefKind::Result(r) => format!(
                        "::core::result::Result<{}, {}>",
                        r.ok.map(|t| self.param(from, t)).unwrap_or_else(|| "()".into()),
                        r.err.map(|t| self.param(from, t)).unwrap_or_else(|| "()".into())
                    ),
                    TypeDefKind::Tuple(tu) => tuple(tu.types.iter().map(|t| self.param(from, *t))),
                    TypeDefKind::Handle(Handle::Own(r)) => self.named_path(from, *r),
                    TypeDefKind::Handle(Handle::Borrow(r)) => format!("&{}", self.named_path(from, *r)),
                    TypeDefKind::Type(t) => self.param(from, *t),
                    _ => self.owned(from, ty),
                }
            }
            _ => self.owned(from, ty),
        }
    }

    /// The `#[repr(C)]` form of a type.
    fn abi(&self, from: InterfaceId, ty: Type) -> String {
        if let Type::Id(id) = self.wit.dealias(ty) {
            if matches!(self.wit.resolve.types[id].kind, TypeDefKind::Handle(_) | TypeDefKind::Resource) {
                return "u32".into();
            }
        }
        format!("<{} as {RT}::lift::Abi>::Abi", self.owned(from, ty))
    }

    /// No strings, lists or handles anywhere: the Rust type is `Copy`.
    fn is_copy(&self, ty: Type) -> bool {
        match self.wit.dealias(ty) {
            Type::String => false,
            Type::Id(id) => match &self.wit.resolve.types[id].kind {
                TypeDefKind::Record(r) => r.fields.iter().all(|f| self.is_copy(f.ty)),
                TypeDefKind::Tuple(t) => t.types.iter().all(|t| self.is_copy(*t)),
                TypeDefKind::Variant(v) => v.cases.iter().filter_map(|c| c.ty).all(|t| self.is_copy(t)),
                TypeDefKind::Option(t) => self.is_copy(*t),
                TypeDefKind::Result(r) => r.ok.iter().chain(r.err.iter()).all(|t| self.is_copy(*t)),
                TypeDefKind::List(_) | TypeDefKind::Handle(_) | TypeDefKind::Resource => false,
                TypeDefKind::Type(t) => self.is_copy(*t),
                _ => true,
            },
            _ => true,
        }
    }

    fn has_handles(&self, ty: Type) -> bool {
        match self.wit.dealias(ty) {
            Type::Id(id) => match &self.wit.resolve.types[id].kind {
                TypeDefKind::Record(r) => r.fields.iter().any(|f| self.has_handles(f.ty)),
                TypeDefKind::Tuple(t) => t.types.iter().any(|t| self.has_handles(*t)),
                TypeDefKind::Variant(v) => v.cases.iter().filter_map(|c| c.ty).any(|t| self.has_handles(t)),
                TypeDefKind::Option(t) | TypeDefKind::List(t) => self.has_handles(*t),
                TypeDefKind::Result(r) => r.ok.iter().chain(r.err.iter()).any(|t| self.has_handles(*t)),
                TypeDefKind::Handle(_) | TypeDefKind::Resource => true,
                TypeDefKind::Type(t) => self.has_handles(*t),
                _ => false,
            },
            _ => false,
        }
    }

    fn derives(&self, ty: Type, base: &[&str]) -> String {
        let mut d: Vec<String> = base.iter().map(|s| s.to_string()).collect();
        if !self.has_handles(ty) {
            d.insert(0, "Clone".into());
            if self.is_copy(ty) {
                d.insert(1, "Copy".into());
            }
            for x in &self.opts.additional_derives {
                if !d.contains(x) {
                    d.push(x.clone());
                }
            }
        }
        format!("#[derive({})]", d.join(", "))
    }

    fn import_index(&self, iface: InterfaceId) -> Option<usize> {
        self.imports.iter().position(|&i| i == iface)
    }

    fn resource_index(&self, r: TypeId) -> usize {
        let r = self.wit.dealias_id(r);
        self.exported_resources().iter().position(|&x| x == r).expect("exported resource")
    }

    fn exported_resources(&self) -> Vec<TypeId> {
        let mut v = Vec::new();
        for &i in &self.exports {
            for &t in self.wit.resolve.interfaces[i].types.values() {
                if matches!(self.wit.resolve.types[t].kind, TypeDefKind::Resource) {
                    v.push(t);
                }
            }
        }
        v
    }

    // -----------------------------------------------------------------------

    fn interface(&self, iface: InterfaceId) -> Result<String> {
        let dir = self.dirs[&iface];
        let i = &self.wit.resolve.interfaces[iface];
        let mut s = String::new();
        s.push_str(&docs(&i.docs, ""));
        writeln!(s, "#[allow(unused_imports)] use {RT}::lift::{{Abi as _, LiftParam as _, LiftResult as _, LowerParam as _, LowerResult as _}};").unwrap();
        let callable = self.import_index(iface).is_some() || dir == Dir::Export;
        for (name, &tid) in &i.types {
            self.type_def(iface, name, tid, &mut s)?;
        }
        if !callable {
            return Ok(s);
        }
        let slots = slots(self.wit, iface);
        for slot in &slots {
            if let Some(f) = slot.func() {
                self.args_struct(iface, slot, f, &mut s);
            }
        }
        match dir {
            Dir::Import => {
                for slot in &slots {
                    if let Some(f) = slot.func() {
                        if resource_of_func(f).is_none() {
                            s.push_str(&docs(&f.docs, ""));
                            s.push_str(&self.import_fn(iface, slot, f, ""));
                        }
                    }
                }
            }
            Dir::Export => self.export_side(iface, &slots, &mut s)?,
        }
        Ok(s)
    }

    fn type_def(&self, iface: InterfaceId, name: &str, tid: TypeId, s: &mut String) -> Result<()> {
        let t = &self.wit.resolve.types[tid];
        let rname = rs_type_name(name);
        let ty = Type::Id(tid);
        s.push_str(&docs(&t.docs, ""));
        match &t.kind {
            TypeDefKind::Type(inner) => {
                writeln!(s, "pub type {rname} = {};", self.owned(iface, *inner)).unwrap();
                if let Type::Id(r) = self.wit.dealias(*inner) {
                    if matches!(self.wit.resolve.types[r].kind, TypeDefKind::Resource) && self.resource_dir(r) == Dir::Export {
                        writeln!(s, "pub type {rname}Borrow<'a> = {}Borrow<'a>;", self.owned(iface, *inner)).unwrap();
                    }
                }
            }
            TypeDefKind::Record(r) => {
                writeln!(s, "{}\npub struct {rname} {{", self.derives(ty, &["Debug"])).unwrap();
                for f in &r.fields {
                    s.push_str(&docs(&f.docs, "    "));
                    writeln!(s, "    pub {}: {},", rs_ident(&f.name), self.owned(iface, f.ty)).unwrap();
                }
                writeln!(s, "}}").unwrap();
                writeln!(s, "#[doc(hidden)] #[repr(C)] #[derive(Clone, Copy)]\npub struct __{rname}Abi {{").unwrap();
                for f in &r.fields {
                    writeln!(s, "    pub {}: {},", rs_ident(&f.name), self.abi(iface, f.ty)).unwrap();
                }
                writeln!(s, "}}").unwrap();
                let fields: Vec<_> = r.fields.iter().map(|f| rs_ident(&f.name)).collect();
                let build = |expr: &dyn Fn(&str) -> String| fields.iter().map(|f| format!("{f}: {}", expr(f))).collect::<Vec<_>>().join(", ");
                writeln!(
                    s,
                    "unsafe impl {RT}::lift::Abi for {rname} {{ type Abi = __{rname}Abi; }}
impl {RT}::lift::LowerParam for {rname} {{
    fn lower_param(&self, s: &{RT}::lift::Scratch) -> __{rname}Abi {{ __{rname}Abi {{ {} }} }}
}}
impl {RT}::lift::LiftParam for {rname} {{
    unsafe fn lift_param(a: &__{rname}Abi) -> ::core::result::Result<Self, {RT}::Trap> {{ unsafe {{ Ok({rname} {{ {} }}) }} }}
}}
impl {RT}::lift::LowerResult for {rname} {{
    fn lower_result(self, cx: &mut {RT}::lift::AllocCx) -> ::core::result::Result<__{rname}Abi, {RT}::Trap> {{ Ok(__{rname}Abi {{ {} }}) }}
}}
impl {RT}::lift::LiftResult for {rname} {{
    unsafe fn lift_result(a: __{rname}Abi) -> ::core::result::Result<Self, {RT}::Trap> {{ unsafe {{ Ok({rname} {{ {} }}) }} }}
}}",
                    build(&|f| format!("{RT}::lift::LowerParam::lower_param(&self.{f}, s)")),
                    build(&|f| format!("{RT}::lift::LiftParam::lift_param(&a.{f})?")),
                    build(&|f| format!("{RT}::lift::LowerResult::lower_result(self.{f}, cx)?")),
                    build(&|f| format!("{RT}::lift::LiftResult::lift_result(a.{f})?")),
                )
                .unwrap();
            }
            TypeDefKind::Variant(v) => {
                writeln!(s, "{}\npub enum {rname} {{", self.derives(ty, &["Debug"])).unwrap();
                for c in &v.cases {
                    s.push_str(&docs(&c.docs, "    "));
                    match c.ty {
                        Some(t) => writeln!(s, "    {}({}),", rs_type_name(&c.name), self.owned(iface, t)).unwrap(),
                        None => writeln!(s, "    {},", rs_type_name(&c.name)).unwrap(),
                    }
                }
                writeln!(s, "}}").unwrap();
                self.variant_abi(iface, &rname, discriminant_size(v.cases.len()), &v.cases.iter().map(|c| (rs_type_name(&c.name), rs_ident(&c.name), c.ty)).collect::<Vec<_>>(), s);
                self.error_impls(tid, &rname, s);
            }
            TypeDefKind::Enum(e) => {
                let int = ["u8", "u16", "", "u32"][discriminant_size(e.cases.len()) - 1];
                writeln!(s, "#[repr({int})]\n#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]\npub enum {rname} {{").unwrap();
                for c in &e.cases {
                    s.push_str(&docs(&c.docs, "    "));
                    writeln!(s, "    {},", rs_type_name(&c.name)).unwrap();
                }
                writeln!(s, "}}").unwrap();
                let arms: String = e.cases.iter().enumerate().map(|(n, c)| format!("{n} => Ok({rname}::{}), ", rs_type_name(&c.name))).collect();
                writeln!(
                    s,
                    "impl {rname} {{
    fn __lift(v: {int}) -> ::core::result::Result<Self, {RT}::Trap> {{ match v {{ {arms}_ => Err({RT}::Trap::invalid(::std::format!(\"{name}: case {{v}}\"))) }} }}
}}
unsafe impl {RT}::lift::Abi for {rname} {{ type Abi = {int}; }}
impl {RT}::lift::LowerParam for {rname} {{ fn lower_param(&self, _: &{RT}::lift::Scratch) -> {int} {{ *self as {int} }} }}
impl {RT}::lift::LiftParam for {rname} {{ unsafe fn lift_param(a: &{int}) -> ::core::result::Result<Self, {RT}::Trap> {{ Self::__lift(*a) }} }}
impl {RT}::lift::LowerResult for {rname} {{ fn lower_result(self, _: &mut {RT}::lift::AllocCx) -> ::core::result::Result<{int}, {RT}::Trap> {{ Ok(self as {int}) }} }}
impl {RT}::lift::LiftResult for {rname} {{ unsafe fn lift_result(a: {int}) -> ::core::result::Result<Self, {RT}::Trap> {{ Self::__lift(a) }} }}"
                )
                .unwrap();
                self.error_impls(tid, &rname, s);
            }
            TypeDefKind::Flags(f) => {
                let int = ["u8", "u16", "", "u32"][flags_size(f.flags.len()) - 1];
                writeln!(s, "{RT}::__private::bitflags::bitflags! {{\n    #[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Clone, Copy)]\n    pub struct {rname}: {int} {{").unwrap();
                for (n, fl) in f.flags.iter().enumerate() {
                    writeln!(s, "        const {} = 1 << {n};", rs_ident(&fl.name).to_uppercase()).unwrap();
                }
                writeln!(s, "    }}\n}}").unwrap();
                writeln!(
                    s,
                    "unsafe impl {RT}::lift::Abi for {rname} {{ type Abi = {int}; }}
impl {RT}::lift::LowerParam for {rname} {{ fn lower_param(&self, _: &{RT}::lift::Scratch) -> {int} {{ self.bits() }} }}
impl {RT}::lift::LiftParam for {rname} {{ unsafe fn lift_param(a: &{int}) -> ::core::result::Result<Self, {RT}::Trap> {{ Ok(Self::from_bits_retain(*a)) }} }}
impl {RT}::lift::LowerResult for {rname} {{ fn lower_result(self, _: &mut {RT}::lift::AllocCx) -> ::core::result::Result<{int}, {RT}::Trap> {{ Ok(self.bits()) }} }}
impl {RT}::lift::LiftResult for {rname} {{ unsafe fn lift_result(a: {int}) -> ::core::result::Result<Self, {RT}::Trap> {{ Ok(Self::from_bits_retain(a)) }} }}"
                )
                .unwrap();
            }
            TypeDefKind::Resource => match self.dirs[&iface] {
                Dir::Import => self.import_resource(iface, tid, &rname, s),
                Dir::Export => self.export_resource(iface, tid, &rname, s),
            },
            k => bail!("unsupported named type {name}: {k:?}"),
        }
        Ok(())
    }

    fn error_impls(&self, tid: TypeId, rname: &str, s: &mut String) {
        if self.err_types.contains(&tid) {
            writeln!(
                s,
                "impl ::core::fmt::Display for {rname} {{ fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {{ write!(f, \"{{:?}}\", self) }} }}
impl ::std::error::Error for {rname} {{}}"
            )
            .unwrap();
        }
    }

    /// `#[repr(C)] struct { tag; union payload }` plus the four conversions.
    fn variant_abi(&self, iface: InterfaceId, rname: &str, disc: usize, cases: &[(String, String, Option<Type>)], s: &mut String) {
        let int = ["u8", "u16", "", "u32"][disc - 1];
        let has_payload = cases.iter().any(|c| c.2.is_some());
        writeln!(s, "#[doc(hidden)] #[repr(C)] #[derive(Clone, Copy)]\npub struct __{rname}Abi {{ pub tag: {int},{} }}", if has_payload { format!(" pub val: __{rname}Payload") } else { String::new() }).unwrap();
        if has_payload {
            writeln!(s, "#[doc(hidden)] #[repr(C)] #[derive(Clone, Copy)]\npub union __{rname}Payload {{").unwrap();
            for (_, field, ty) in cases {
                if let Some(t) = ty {
                    writeln!(s, "    pub {field}: {},", self.abi(iface, *t)).unwrap();
                }
            }
            writeln!(s, "}}").unwrap();
        }
        let lower = |method: &str, recv: &str, extra: &str, wrap: &str, q: &str| -> String {
            cases
                .iter()
                .enumerate()
                .map(|(n, (case, field, ty))| match ty {
                    Some(_) => format!(
                        "{rname}::{case}(v) => {wrap}__{rname}Abi {{ tag: {n}, val: __{rname}Payload {{ {field}: {RT}::lift::{method}({recv}v, {extra}){q} }} }}{}, ",
                        if wrap.is_empty() { "" } else { ")" }
                    ),
                    None => format!(
                        "{rname}::{case} => {{ let mut a: __{rname}Abi = unsafe {{ ::core::mem::zeroed() }}; a.tag = {n}; {wrap}a{} }} ",
                        if wrap.is_empty() { "" } else { ")" }
                    ),
                })
                .collect()
        };
        let lift = |method: &str, recv: &str| -> String {
            cases
                .iter()
                .enumerate()
                .map(|(n, (case, field, ty))| match ty {
                    Some(_) => format!("{n} => {rname}::{case}({RT}::lift::{method}({recv}a.val.{field})?), "),
                    None => format!("{n} => {rname}::{case}, "),
                })
                .collect()
        };
        writeln!(
            s,
            "unsafe impl {RT}::lift::Abi for {rname} {{ type Abi = __{rname}Abi; }}
impl {RT}::lift::LowerParam for {rname} {{
    fn lower_param(&self, s: &{RT}::lift::Scratch) -> __{rname}Abi {{ match self {{ {} }} }}
}}
impl {RT}::lift::LiftParam for {rname} {{
    unsafe fn lift_param(a: &__{rname}Abi) -> ::core::result::Result<Self, {RT}::Trap> {{ unsafe {{ Ok(match a.tag {{ {}t => return Err({RT}::Trap::invalid(::std::format!(\"{rname}: case {{t}}\"))) }}) }} }}
}}
impl {RT}::lift::LowerResult for {rname} {{
    fn lower_result(self, cx: &mut {RT}::lift::AllocCx) -> ::core::result::Result<__{rname}Abi, {RT}::Trap> {{ match self {{ {} }} }}
}}
impl {RT}::lift::LiftResult for {rname} {{
    unsafe fn lift_result(a: __{rname}Abi) -> ::core::result::Result<Self, {RT}::Trap> {{ unsafe {{ Ok(match a.tag {{ {}t => return Err({RT}::Trap::invalid(::std::format!(\"{rname}: case {{t}}\"))) }}) }} }}
}}",
            lower("LowerParam::lower_param", "", "s", "", ""),
            lift("LiftParam::lift_param", "&"),
            lower("LowerResult::lower_result", "", "cx", "Ok(", "?"),
            lift("LiftResult::lift_result", ""),
        )
        .unwrap();
    }

    fn args_struct(&self, iface: InterfaceId, slot: &Slot, f: &Function, s: &mut String) {
        if f.params.is_empty() {
            return;
        }
        writeln!(s, "#[doc(hidden)] #[repr(C)] #[derive(Clone, Copy)]\npub struct __{}Args {{", rs_type_name(&slot.name)).unwrap();
        for p in &f.params {
            writeln!(s, "    pub {}: {},", rs_ident(&p.name), self.abi(iface, p.ty)).unwrap();
        }
        writeln!(s, "}}").unwrap();
    }

    fn what(&self, iface: InterfaceId, f: &Function) -> String {
        format!("{}.{}", self.wit.iface_name(iface).unwrap(), f.name)
    }

    /// An import call. `recv` is `self.` for resource methods/drops that use
    /// the interface captured in the handle wrapper.
    fn import_fn(&self, iface: InterfaceId, slot: &Slot, f: &Function, indent: &str) -> String {
        let world = self.world_path(iface);
        let idx = self.import_index(iface).unwrap();
        let mut sig = Vec::new();
        let mut lower = Vec::new();
        let mut owns = Vec::new();
        let is_method = matches!(f.kind, FunctionKind::Method(_));
        for (n, p) in f.params.iter().enumerate() {
            let name = rs_ident(&p.name);
            if is_method && n == 0 {
                sig.push("&self".to_string());
                lower.push(format!("{name}: self.handle()"));
                continue;
            }
            sig.push(format!("{name}: {}", self.param(iface, p.ty)));
            match self.handle_kind(p.ty) {
                Some(Handle::Own(_)) => {
                    owns.push(format!("let {name} = ::core::mem::ManuallyDrop::new({name});"));
                    lower.push(format!("{name}: {name}.handle()"));
                }
                Some(Handle::Borrow(_)) => lower.push(format!("{name}: {name}.handle()")),
                None => lower.push(format!("{name}: {RT}::lift::LowerParam::lower_param(&{name}, &__s)")),
            }
        }
        let (fname, ret_ty) = match f.kind {
            FunctionKind::Constructor(_) => ("new".to_string(), "Self".to_string()),
            _ => (rs_ident(f.item_name()), f.result.map(|r| self.owned(iface, r)).unwrap_or_else(|| "()".into())),
        };
        let iface_expr = if is_method { "self.__iface".to_string() } else { format!("{world}::import({idx})") };
        let args = if f.params.is_empty() {
            "::core::ptr::null()".to_string()
        } else {
            format!("&__{}Args {{ {} }} as *const _ as *const ::core::ffi::c_void", rs_type_name(&slot.name), lower.join(", "))
        };
        let what = self.what(iface, f);
        let mut b = String::new();
        writeln!(b, "{indent}pub fn {fname}({}) -> {ret_ty} {{", sig.join(", ")).unwrap();
        writeln!(b, "{indent}    unsafe {{").unwrap();
        for o in owns {
            writeln!(b, "{indent}        {o}").unwrap();
        }
        writeln!(b, "{indent}        let __iface = {iface_expr};\n{indent}        let __s = {RT}::lift::Scratch::new();").unwrap();
        match f.result {
            Some(r) => {
                let abi = self.abi(iface, r);
                writeln!(b, "{indent}        let mut __ret = ::core::mem::MaybeUninit::<{abi}>::uninit();").unwrap();
                writeln!(b, "{indent}        if let Err(t) = __iface.invoke({}, {args}, __ret.as_mut_ptr() as *mut ::core::ffi::c_void, &{RT}::lift::RUST_ALLOC) {{ {RT}::trap::raise(t, \"{what}\") }}", slot.offset).unwrap();
                if matches!(f.kind, FunctionKind::Constructor(_)) {
                    writeln!(b, "{indent}        Self::from_handle(__ret.assume_init())").unwrap();
                } else {
                    writeln!(b, "{indent}        match {RT}::lift::LiftResult::lift_result(__ret.assume_init()) {{ Ok(v) => v, Err(t) => {RT}::trap::raise(t, \"{what}\") }}").unwrap();
                }
            }
            None => {
                writeln!(b, "{indent}        if let Err(t) = __iface.invoke({}, {args}, ::core::ptr::null_mut(), &{RT}::lift::RUST_ALLOC) {{ {RT}::trap::raise(t, \"{what}\") }}", slot.offset).unwrap();
            }
        }
        writeln!(b, "{indent}    }}\n{indent}}}").unwrap();
        b
    }

    fn handle_kind(&self, ty: Type) -> Option<Handle> {
        match self.wit.dealias(ty) {
            Type::Id(id) => match self.wit.resolve.types[id].kind {
                TypeDefKind::Handle(h) => Some(h),
                _ => None,
            },
            _ => None,
        }
    }

    fn import_resource(&self, iface: InterfaceId, tid: TypeId, rname: &str, s: &mut String) {
        let world = self.world_path(iface);
        let idx = self.import_index(iface);
        let slots = slots(self.wit, iface);
        let drop = slots.iter().find(|sl| matches!(sl.kind, SlotKind::Drop(r) if r == tid)).map(|sl| sl.offset);
        writeln!(
            s,
            "/// An owned handle to a resource implemented by another component.
#[derive(Debug)]
pub struct {rname} {{ __handle: ::core::sync::atomic::AtomicU32, __iface: {RT}::abi::Iface }}
impl {rname} {{
    /// # Safety
    /// `handle` must be an owned handle to this resource type.
    pub unsafe fn from_handle(handle: u32) -> Self {{ Self {{ __handle: handle.into(), __iface: {} }} }}
    pub fn handle(&self) -> u32 {{ self.__handle.load(::core::sync::atomic::Ordering::Relaxed) }}
    /// Gives up ownership: the handle will not be dropped by this wrapper.
    pub fn take_handle(&self) -> u32 {{ self.__handle.swap(0, ::core::sync::atomic::Ordering::Relaxed) }}
}}
impl Drop for {rname} {{
    fn drop(&mut self) {{
        let h = self.take_handle();
        if h != 0 {{ let _ = unsafe {{ self.__iface.invoke({}, &h as *const u32 as *const ::core::ffi::c_void, ::core::ptr::null_mut(), &{RT}::lift::RUST_ALLOC) }}; }}
    }}
}}
unsafe impl {RT}::lift::Abi for {rname} {{ type Abi = u32; }}
impl {RT}::lift::LowerParam for {rname} {{ fn lower_param(&self, _: &{RT}::lift::Scratch) -> u32 {{ self.handle() }} }}
impl {RT}::lift::LiftParam for {rname} {{ unsafe fn lift_param(a: &u32) -> ::core::result::Result<Self, {RT}::Trap> {{ unsafe {{ Ok(Self::from_handle(*a)) }} }} }}
impl {RT}::lift::LowerResult for {rname} {{ fn lower_result(self, _: &mut {RT}::lift::AllocCx) -> ::core::result::Result<u32, {RT}::Trap> {{ Ok(self.take_handle()) }} }}
impl {RT}::lift::LiftResult for {rname} {{ unsafe fn lift_result(a: u32) -> ::core::result::Result<Self, {RT}::Trap> {{ unsafe {{ Ok(Self::from_handle(a)) }} }} }}",
            match idx {
                Some(i) => format!("{world}::import({i})"),
                None => format!("{RT}::abi::Iface::NULL"),
            },
            drop.unwrap_or(0),
        )
        .unwrap();
        if idx.is_none() {
            return;
        }
        writeln!(s, "impl {rname} {{").unwrap();
        for slot in &slots {
            if let Some(f) = slot.func() {
                if resource_of_func(f) == Some(tid) {
                    s.push_str(&docs(&f.docs, "    "));
                    s.push_str(&self.import_fn(iface, slot, f, "    "));
                }
            }
        }
        writeln!(s, "}}").unwrap();
    }

    fn export_resource(&self, iface: InterfaceId, tid: TypeId, rname: &str, s: &mut String) {
        let world = self.world_path(iface);
        let ridx = self.resource_index(tid);
        writeln!(
            s,
            "/// An owned handle to a resource this component exports.
pub struct {rname} {{ __handle: ::core::sync::atomic::AtomicU32, __obj: {RT}::component::AnyArc, __inst: &'static {RT}::component::Instance }}
impl ::core::fmt::Debug for {rname} {{
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {{ f.debug_struct(\"{rname}\").field(\"handle\", &self.handle()).finish() }}
}}
impl {rname} {{
    /// Wraps a new resource object in a handle owned by this component.
    pub fn new<T: Guest{rname}>(val: T) -> Self {{
        let inst = {world}::instance();
        let obj: {RT}::component::AnyArc = ::std::sync::Arc::new(val);
        let h = inst.resources[{ridx}].insert(obj.clone());
        Self {{ __handle: h.into(), __obj: obj, __inst: inst }}
    }}
    pub fn get<T: Guest{rname}>(&self) -> &T {{ self.__obj.downcast_ref::<T>().expect(\"{rname}: wrong implementation type\") }}
    pub fn into_inner<T: Guest{rname}>(self) -> T {{
        let h = self.take_handle();
        if h != 0 {{ let _ = self.__inst.resources[{ridx}].remove(h); }}
        let obj = self.__obj.clone();
        drop(self);
        match ::std::sync::Arc::downcast::<T>(obj) {{
            Ok(a) => ::std::sync::Arc::try_unwrap(a).unwrap_or_else(|_| panic!(\"{rname}: still in use\")),
            Err(_) => panic!(\"{rname}: wrong implementation type\"),
        }}
    }}
    pub fn handle(&self) -> u32 {{ self.__handle.load(::core::sync::atomic::Ordering::Relaxed) }}
    pub fn take_handle(&self) -> u32 {{ self.__handle.swap(0, ::core::sync::atomic::Ordering::Relaxed) }}
    #[doc(hidden)]
    pub fn __lift_own(h: u32) -> ::core::result::Result<Self, {RT}::Trap> {{
        let inst = {world}::instance();
        let obj = inst.resources[{ridx}].get(h)?;
        Ok(Self {{ __handle: h.into(), __obj: obj, __inst: inst }})
    }}
}}
impl Drop for {rname} {{
    fn drop(&mut self) {{ let h = self.take_handle(); if h != 0 {{ let _ = self.__inst.resources[{ridx}].remove(h); }} }}
}}
/// A borrowed handle to a resource this component exports, valid for one call.
pub struct {rname}Borrow<'a> {{ __obj: {RT}::component::AnyArc, __handle: u32, _m: ::core::marker::PhantomData<&'a ()> }}
impl<'a> {rname}Borrow<'a> {{
    pub fn get<T: Guest{rname}>(&self) -> &T {{ self.__obj.downcast_ref::<T>().expect(\"{rname}: wrong implementation type\") }}
    pub fn handle(&self) -> u32 {{ self.__handle }}
    #[doc(hidden)]
    pub fn __lift(h: u32) -> ::core::result::Result<Self, {RT}::Trap> {{
        Ok(Self {{ __obj: {world}::instance().resources[{ridx}].get(h)?, __handle: h, _m: ::core::marker::PhantomData }})
    }}
}}
impl ::core::fmt::Debug for {rname}Borrow<'_> {{
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {{ f.debug_struct(\"{rname}Borrow\").field(\"handle\", &self.__handle).finish() }}
}}
unsafe impl {RT}::lift::Abi for {rname} {{ type Abi = u32; }}
impl {RT}::lift::LowerParam for {rname} {{ fn lower_param(&self, _: &{RT}::lift::Scratch) -> u32 {{ self.handle() }} }}
impl {RT}::lift::LiftParam for {rname} {{ unsafe fn lift_param(a: &u32) -> ::core::result::Result<Self, {RT}::Trap> {{ Self::__lift_own(*a) }} }}
impl {RT}::lift::LowerResult for {rname} {{ fn lower_result(self, _: &mut {RT}::lift::AllocCx) -> ::core::result::Result<u32, {RT}::Trap> {{ Ok(self.take_handle()) }} }}
impl {RT}::lift::LiftResult for {rname} {{ unsafe fn lift_result(a: u32) -> ::core::result::Result<Self, {RT}::Trap> {{ Self::__lift_own(a) }} }}"
        )
        .unwrap();
    }

    fn export_side(&self, iface: InterfaceId, slots: &[Slot], s: &mut String) -> Result<()> {
        let i = &self.wit.resolve.interfaces[iface];
        let world = self.world_path(iface);
        let resources: Vec<(TypeId, String)> = i
            .types
            .iter()
            .filter(|(_, t)| matches!(self.wit.resolve.types[**t].kind, TypeDefKind::Resource))
            .map(|(n, t)| (*t, rs_type_name(n)))
            .collect();

        // Guest traits.
        writeln!(s, "pub trait Guest {{").unwrap();
        for (_, r) in &resources {
            writeln!(s, "    type {r}: Guest{r};").unwrap();
        }
        for slot in slots {
            if let Some(f) = slot.func() {
                if resource_of_func(f).is_none() {
                    s.push_str(&docs(&f.docs, "    "));
                    writeln!(s, "    fn {}({}) -> {};", rs_ident(&f.name), self.export_params(iface, f, false).join(", "), self.ret(iface, f)).unwrap();
                }
            }
        }
        writeln!(s, "}}").unwrap();
        for (tid, r) in &resources {
            writeln!(s, "pub trait Guest{r}: ::core::marker::Send + ::core::marker::Sync + 'static {{").unwrap();
            for slot in slots {
                let Some(f) = slot.func() else { continue };
                if resource_of_func(f) != Some(*tid) {
                    continue;
                }
                s.push_str(&docs(&f.docs, "    "));
                let params = self.export_params(iface, f, true);
                match f.kind {
                    FunctionKind::Constructor(_) => writeln!(s, "    fn new({}) -> Self where Self: Sized;", params.join(", ")).unwrap(),
                    FunctionKind::Method(_) => writeln!(s, "    fn {}({}) -> {};", rs_ident(f.item_name()), params.join(", "), self.ret(iface, f)).unwrap(),
                    _ => writeln!(s, "    fn {}({}) -> {} where Self: Sized;", rs_ident(f.item_name()), params.join(", "), self.ret(iface, f)).unwrap(),
                }
            }
            writeln!(s, "}}").unwrap();
        }

        // Trampolines.
        for slot in slots {
            let name = format!("__export_{}", slot.name);
            writeln!(
                s,
                "#[doc(hidden)]
pub unsafe extern \"C\" fn {name}<G: Guest>(inst: *mut ::core::ffi::c_void, args: *const ::core::ffi::c_void, ret: *mut ::core::ffi::c_void, alloc: *const {RT}::abi::Alloc) -> i32 {{
    {RT}::trap::guard(|| {world}::enter(inst, || unsafe {{"
            )
            .unwrap();
            match slot.kind {
                SlotKind::Drop(r) => {
                    writeln!(s, "        {world}::instance().resources[{}].remove(*(args as *const u32))?;\n        Ok(())", self.resource_index(r)).unwrap();
                }
                SlotKind::Func(f) => self.export_body(iface, slot, f, s),
            }
            writeln!(s, "    }}))\n}}").unwrap();
        }

        let n = slots.len();
        writeln!(
            s,
            "#[doc(hidden)]
pub fn __table<G: Guest>() -> &'static {RT}::abi::Table<{n}> {{
    const {{ &{RT}::abi::Table::new([{}]) }}
}}",
            slots.iter().map(|sl| format!("__export_{}::<G> as {RT}::abi::RawFn", sl.name)).collect::<Vec<_>>().join(", ")
        )
        .unwrap();
        Ok(())
    }

    fn ret(&self, iface: InterfaceId, f: &Function) -> String {
        f.result.map(|r| self.owned(iface, r)).unwrap_or_else(|| "()".into())
    }

    fn export_params(&self, iface: InterfaceId, f: &Function, in_resource: bool) -> Vec<String> {
        f.params
            .iter()
            .enumerate()
            .map(|(n, p)| {
                if in_resource && n == 0 && matches!(f.kind, FunctionKind::Method(_)) {
                    "&self".to_string()
                } else {
                    format!("{}: {}", rs_ident(&p.name), self.owned(iface, p.ty))
                }
            })
            .collect()
    }

    fn export_body(&self, iface: InterfaceId, slot: &Slot, f: &Function, s: &mut String) {
        let world = self.world_path(iface);
        if !f.params.is_empty() {
            writeln!(s, "        let __a = &*(args as *const __{}Args);", rs_type_name(&slot.name)).unwrap();
        }
        let mut call_args = Vec::new();
        let is_method = matches!(f.kind, FunctionKind::Method(_));
        let res = resource_of_func(f);
        for (n, p) in f.params.iter().enumerate() {
            let name = rs_ident(&p.name);
            if is_method && n == 0 {
                let r = res.unwrap();
                writeln!(
                    s,
                    "        let __obj = {world}::instance().resources[{}].get(__a.{name})?;\n        let __self = __obj.downcast_ref::<G::{}>().ok_or_else(|| {RT}::Trap::bad_handle(__a.{name}))?;",
                    self.resource_index(r),
                    rs_type_name(self.wit.resolve.types[r].name.as_ref().unwrap())
                )
                .unwrap();
                continue;
            }
            match self.handle_kind(p.ty) {
                Some(Handle::Borrow(r)) if self.resource_dir(r) == Dir::Export => {
                    writeln!(s, "        let {name} = {}Borrow::__lift(__a.{name})?;", self.named_path(iface, r)).unwrap();
                    call_args.push(name);
                }
                Some(Handle::Borrow(r)) => {
                    writeln!(s, "        let {name} = ::core::mem::ManuallyDrop::new({}::from_handle(__a.{name}));", self.named_path(iface, r)).unwrap();
                    call_args.push(format!("&*{name}"));
                }
                _ => {
                    writeln!(s, "        let {name}: {} = {RT}::lift::LiftParam::lift_param(&__a.{name})?;", self.owned(iface, p.ty)).unwrap();
                    call_args.push(name);
                }
            }
        }
        let call = match (f.kind.clone(), res) {
            (FunctionKind::Constructor(r), _) => format!("<G::{0} as Guest{0}>::new({1})", rs_type_name(self.wit.resolve.types[r].name.as_ref().unwrap()), call_args.join(", ")),
            (FunctionKind::Method(r), _) => format!("<G::{0} as Guest{0}>::{1}(__self{2})", rs_type_name(self.wit.resolve.types[r].name.as_ref().unwrap()), rs_ident(f.item_name()), call_args.iter().map(|a| format!(", {a}")).collect::<String>()),
            (FunctionKind::Static(r), _) => format!("<G::{0} as Guest{0}>::{1}({2})", rs_type_name(self.wit.resolve.types[r].name.as_ref().unwrap()), rs_ident(f.item_name()), call_args.join(", ")),
            _ => format!("<G as Guest>::{}({})", rs_ident(&f.name), call_args.join(", ")),
        };
        match (f.result, &f.kind) {
            (Some(_), FunctionKind::Constructor(r)) => {
                writeln!(
                    s,
                    "        let __v = {call};\n        let __h = {world}::instance().resources[{}].insert(::std::sync::Arc::new(__v));\n        (ret as *mut u32).write(__h);\n        Ok(())",
                    self.resource_index(*r)
                )
                .unwrap();
            }
            (Some(r), _) => {
                writeln!(
                    s,
                    "        let __r = {call};\n        let mut __cx = {RT}::lift::AllocCx::new(&*alloc);\n        let __v: {} = {RT}::lift::LowerResult::lower_result(__r, &mut __cx)?;\n        (ret as *mut {}).write(__v);\n        __cx.commit();\n        Ok(())",
                    self.abi(iface, r),
                    self.abi(iface, r)
                )
                .unwrap();
            }
            (None, _) => writeln!(s, "        {call};\n        Ok(())").unwrap(),
        }
    }

    // -----------------------------------------------------------------------

    fn world_module(&self, world: WorldId) -> String {
        let w = &self.wit.resolve.worlds[world];
        let name = &self.opts.name;
        let sym = symbol(name);
        let mut s = String::new();
        let imports: Vec<String> = self.imports.iter().map(|&i| self.wit.iface_id(i)).collect();
        let exports: Vec<String> = self.exports.iter().map(|&i| self.wit.iface_id(i)).collect();
        let bounds: Vec<String> = self.exports.iter().map(|&i| format!("super::{}::Guest", self.module_path(i).join("::"))).collect();
        let bound = if bounds.is_empty() { String::new() } else { format!(": {}", bounds.join(" + ")) };
        let cstrs = |v: &[String]| v.iter().map(|x| format!("c\"{x}\".as_ptr()")).collect::<Vec<_>>().join(", ");
        writeln!(
            s,
            "#[doc(hidden)]
#[allow(dead_code, clippy::all)]
pub mod __wna_world {{
    {RT}::__component_context!(\"{name}\");
    pub const NAME: &str = \"{name}\";
    pub const IMPORTS: &[&str] = &[{}];
    pub const EXPORTS: &[&str] = &[{}];
    pub const N_RESOURCES: usize = {};
    const IMPORT_PTRS: &[*const ::core::ffi::c_char] = &[{}];
    const EXPORT_PTRS: &[*const ::core::ffi::c_char] = &[{}];

    pub fn import(i: usize) -> {RT}::abi::Iface {{ instance().imports[i] }}

    pub fn descriptor<T{bound}>() -> &'static {RT}::abi::Component {{
        const {{
            &{RT}::abi::Component {{
                abi_version: {RT}::abi::ABI_VERSION,
                name: c\"{name}\".as_ptr(),
                n_imports: {},
                imports: IMPORT_PTRS.as_ptr(),
                n_exports: {},
                exports: EXPORT_PTRS.as_ptr(),
                instantiate: __instantiate,
                get_export: __get_export::<T>,
                destroy: __destroy,
            }}
        }}
    }}

    unsafe extern \"C\" fn __instantiate(imports: *const {RT}::abi::Iface, out: *mut *mut ::core::ffi::c_void) -> i32 {{
        {RT}::trap::guard(|| unsafe {{
            let imports: &[{RT}::abi::Iface] = if IMPORTS.is_empty() {{ &[] }} else {{ ::core::slice::from_raw_parts(imports, IMPORTS.len()) }};
            for (n, i) in imports.iter().enumerate() {{
                if i.table.is_null() {{ return Err({RT}::Trap::new({RT}::abi::TRAP_UNAVAILABLE, ::std::format!(\"{name}: import {{}} not supplied\", IMPORTS[n]))); }}
            }}
            *out = instantiate(imports, N_RESOURCES) as *mut ::core::ffi::c_void;
            Ok(())
        }})
    }}

    unsafe extern \"C\" fn __get_export<T{bound}>(inst: *mut ::core::ffi::c_void, iface: *const ::core::ffi::c_char, out: *mut {RT}::abi::Iface) -> i32 {{
        let Ok(req) = unsafe {{ ::core::ffi::CStr::from_ptr(iface) }}.to_str() else {{ return {RT}::abi::TRAP_INVALID }};
        #[allow(unreachable_code)]
        let table: *const ::core::ffi::c_void = {} {{ return {RT}::abi::TRAP_UNAVAILABLE }};
        unsafe {{ *out = {RT}::abi::Iface {{ table, inst }} }};
        {RT}::abi::OK
    }}

    unsafe extern \"C\" fn __destroy(_inst: *mut ::core::ffi::c_void) -> i32 {{ {RT}::abi::OK }}
}}
",
            imports.iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(", "),
            exports.iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(", "),
            self.exported_resources().len(),
            cstrs(&imports),
            cstrs(&exports),
            imports.len(),
            exports.len(),
            self.exports
                .iter()
                .enumerate()
                .map(|(n, &i)| format!("if {RT}::component::iface_matches(req, EXPORTS[{n}]) {{ super::{}::__table::<T>() as *const _ as *const ::core::ffi::c_void }} else", self.module_path(i).join("::")))
                .collect::<String>(),
        )
        .unwrap();
        let _ = w;
        writeln!(
            s,
            "/// Exports `$ty` as this component: defines the descriptor symbol `{sym}`.
#[allow(unused_macros)]
macro_rules! __wna_export {{
    ($ty:ident) => {{
        #[unsafe(no_mangle)]
        pub extern \"C\" fn {sym}() -> *const {RT}::abi::Component {{
            self::__wna_world::descriptor::<$ty>()
        }}
    }};
    ($ty:ident with_types_in $($path:tt)*) => {{
        #[unsafe(no_mangle)]
        pub extern \"C\" fn {sym}() -> *const {RT}::abi::Component {{
            $($path)*::__wna_world::descriptor::<$ty>()
        }}
    }};
}}
#[allow(unused_imports)]
pub(crate) use __wna_export as export;"
        )
        .unwrap();
        s
    }
}

fn tuple(items: impl Iterator<Item = String>) -> String {
    let v: Vec<_> = items.collect();
    if v.len() == 1 { format!("({},)", v[0]) } else { format!("({})", v.join(", ")) }
}
