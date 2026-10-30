//! The WNA model shared by every generator backend and by the Wasm bridge:
//! loading a WIT tree, naming, memory layout, and the slot order of
//! function tables.
//!
//! # Binary contract (summary)
//!
//! * Every component exports one descriptor symbol (`<name>__wna_component`).
//! * Every interface is reached through a function table. A table starts with
//!   a [`TABLE_HEADER_SIZE`]-byte header (`size`, `reserved`, `last_trap`) and
//!   is followed by one function pointer per [`Slot`], in [`slots`] order.
//! * Every slot has the same C shape:
//!   `int32_t f(void *inst, const ARGS *args, RET *ret, const wna_alloc *alloc)`
//!   where `ARGS` is a struct of the parameters laid out by [`Layouts`] (a
//!   method's `self` handle first) and `RET` is the result type's layout.
//!   `args`/`ret` are NULL when there are no parameters / no result.

use anyhow::{Context, Result, bail};
use heck::{ToSnakeCase, ToUpperCamelCase};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
pub use wit_parser;
use wit_parser::*;

/// Bytes before the first slot: `uint32_t size; uint32_t reserved; void (*last_trap)(...)`.
pub const TABLE_HEADER_SIZE: usize = 16;
/// Version of the descriptor/table contract.
pub const ABI_VERSION: u32 = 1;

/// A loaded WIT tree: every package directory under one root.
pub struct Wit {
    pub resolve: Resolve,
    /// Every `.wit` file read, for build-script change tracking.
    pub sources: Vec<PathBuf>,
}

impl Wit {
    /// Loads every package found under `root`: `root` itself if it holds
    /// `.wit` files, plus each immediate subdirectory that does. Packages may
    /// depend on each other in any order.
    pub fn load(root: impl AsRef<Path>) -> Result<Wit> {
        let root = root.as_ref();
        let mut dirs = Vec::new();
        if has_wit(root)? {
            dirs.push(root.to_path_buf());
        }
        let mut subdirs: Vec<_> = std::fs::read_dir(root)
            .with_context(|| format!("reading {}", root.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        subdirs.sort();
        for d in subdirs {
            if has_wit(&d)? {
                dirs.push(d);
            }
        }
        if dirs.is_empty() {
            bail!("no .wit packages under {}", root.display());
        }
        let mut resolve = Resolve::default();
        let mut groups = Vec::new();
        let mut sources = Vec::new();
        for d in &dirs {
            for e in std::fs::read_dir(d)? {
                let p = e?.path();
                if p.extension().is_some_and(|e| e == "wit") {
                    sources.push(p);
                }
            }
            groups.push(
                UnresolvedPackageGroup::parse_dir(d)
                    .with_context(|| format!("parsing {}", d.display()))?,
            );
        }
        let main = groups.remove(0);
        resolve
            .push_groups(main, groups)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("resolving WIT under {}", root.display()))?;
        sources.sort();
        Ok(Wit { resolve, sources })
    }

    /// Finds a world by `ns:pkg/world` (version optional) or bare name if unique.
    pub fn world(&self, spec: &str) -> Result<WorldId> {
        let (pkg, name) = match spec.split_once('/') {
            Some((p, n)) => (Some(p), n),
            None => (None, spec),
        };
        let name = name.split('@').next().unwrap();
        let found: Vec<_> = self
            .resolve
            .worlds
            .iter()
            .filter(|(_, w)| w.name == name)
            .filter(|(_, w)| match (pkg, w.package) {
                (Some(p), Some(id)) => {
                    let n = &self.resolve.packages[id].name;
                    p == format!("{}:{}", n.namespace, n.name)
                }
                (Some(_), None) => false,
                (None, _) => true,
            })
            .map(|(id, _)| id)
            .collect();
        match found.as_slice() {
            [id] => Ok(*id),
            [] => bail!("world `{spec}` not found"),
            _ => bail!("world `{spec}` is ambiguous; qualify it as ns:pkg/world"),
        }
    }

    /// Finds an interface by `ns:pkg/iface` (version optional).
    pub fn interface(&self, spec: &str) -> Result<InterfaceId> {
        let base = spec.split('@').next().unwrap();
        self.resolve
            .interfaces
            .iter()
            .find(|(id, _)| self.iface_name(*id).as_deref() == Some(base))
            .map(|(id, _)| id)
            .with_context(|| format!("interface `{spec}` not found"))
    }

    /// `ns:pkg/iface` without version.
    pub fn iface_name(&self, id: InterfaceId) -> Option<String> {
        let i = &self.resolve.interfaces[id];
        let p = &self.resolve.packages[i.package?].name;
        Some(format!("{}:{}/{}", p.namespace, p.name, i.name.as_ref()?))
    }

    /// `ns:pkg/iface@x.y.z`, the string used in descriptors.
    pub fn iface_id(&self, id: InterfaceId) -> String {
        self.resolve.id_of(id).expect("anonymous interfaces are not supported")
    }

    pub fn iface_version(&self, id: InterfaceId) -> Option<semver::Version> {
        let i = &self.resolve.interfaces[id];
        self.resolve.packages[i.package?].name.version.clone()
    }

    /// C identifier prefix of an interface: `acme_core_storage_v2`
    /// (`_v0_3` for 0.x packages; prerelease tags are ignored).
    pub fn prefix(&self, id: InterfaceId) -> String {
        let i = &self.resolve.interfaces[id];
        let p = &self.resolve.packages[i.package.unwrap()].name;
        let mut s = format!(
            "{}_{}_{}",
            ident(&p.namespace),
            ident(&p.name),
            ident(i.name.as_ref().unwrap())
        );
        s.push_str(&version_suffix(p.version.as_ref()));
        s
    }

    /// Resolves `type x = y` aliases down to the underlying type.
    pub fn dealias(&self, mut ty: Type) -> Type {
        while let Type::Id(id) = ty {
            match self.resolve.types[id].kind {
                TypeDefKind::Type(t) => ty = t,
                _ => break,
            }
        }
        ty
    }

    pub fn dealias_id(&self, id: TypeId) -> TypeId {
        match self.dealias(Type::Id(id)) {
            Type::Id(id) => id,
            _ => id,
        }
    }

    /// The resource a handle points to (through aliases).
    pub fn resource_of(&self, handle: &Handle) -> TypeId {
        match handle {
            Handle::Own(r) | Handle::Borrow(r) => self.dealias_id(*r),
        }
    }

    /// Interfaces a world imports and exports, in declaration order. Bare
    /// functions or types at world level are rejected.
    pub fn world_interfaces(&self, world: WorldId) -> Result<(Vec<InterfaceId>, Vec<InterfaceId>)> {
        let w = &self.resolve.worlds[world];
        let collect = |items: &indexmap_like::Items| -> Result<Vec<InterfaceId>> {
            let mut out = Vec::new();
            for (key, item) in items {
                match item {
                    WorldItem::Interface { id, .. } => {
                        if self.resolve.interfaces[*id].name.is_none() {
                            bail!("world `{}`: inline interface `{}` is not supported; name it", w.name, self.resolve.name_world_key(key));
                        }
                        out.push(*id)
                    }
                    WorldItem::Function(f) => bail!("world `{}`: bare function `{}` is not supported; put it in an interface", w.name, f.name),
                    WorldItem::Type { .. } => {}
                }
            }
            Ok(out)
        };
        Ok((collect(&w.imports)?, collect(&w.exports)?))
    }

    /// Rejects types the v0 subset does not support.
    pub fn check_v0(&self, iface: InterfaceId) -> Result<()> {
        let i = &self.resolve.interfaces[iface];
        let name = self.iface_name(iface).unwrap_or_default();
        for f in i.functions.values() {
            match f.kind {
                FunctionKind::Freestanding
                | FunctionKind::Method(_)
                | FunctionKind::Static(_)
                | FunctionKind::Constructor(_) => {}
                _ => bail!("{name}.{}: async functions, getters and setters are not in WNA v0", f.name),
            }
            for p in &f.params {
                self.check_type(&name, &f.name, p.ty)?;
            }
            if let Some(r) = f.result {
                self.check_type(&name, &f.name, r)?;
            }
        }
        Ok(())
    }

    fn check_type(&self, iface: &str, func: &str, ty: Type) -> Result<()> {
        let bad = |what: &str| bail!("{iface}.{func}: {what} is not in WNA v0");
        match self.dealias(ty) {
            Type::ErrorContext => bad("error-context"),
            Type::Id(id) => match &self.resolve.types[id].kind {
                TypeDefKind::Record(r) => r.fields.iter().try_for_each(|f| self.check_type(iface, func, f.ty)),
                TypeDefKind::Tuple(t) => t.types.iter().try_for_each(|t| self.check_type(iface, func, *t)),
                TypeDefKind::Variant(v) => v.cases.iter().filter_map(|c| c.ty).try_for_each(|t| self.check_type(iface, func, t)),
                TypeDefKind::Option(t) | TypeDefKind::List(t) => self.check_type(iface, func, *t),
                TypeDefKind::Result(r) => r.ok.iter().chain(r.err.iter()).try_for_each(|t| self.check_type(iface, func, *t)),
                TypeDefKind::Flags(f) if f.flags.len() > 32 => bad("flags with more than 32 members"),
                TypeDefKind::Map(..) => bad("map"),
                TypeDefKind::FixedLengthList(..) => bad("fixed-length list"),
                TypeDefKind::Future(_) | TypeDefKind::Stream(_) => bad("future/stream"),
                _ => Ok(()),
            },
            _ => Ok(()),
        }
    }
}

mod indexmap_like {
    pub type Items = wit_parser::IndexMap<wit_parser::WorldKey, wit_parser::WorldItem>;
}

pub use semver;

fn has_wit(dir: &Path) -> Result<bool> {
    Ok(std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .any(|e| e.path().extension().is_some_and(|x| x == "wit")))
}

/// `kebab-name` → `kebab_name`.
pub fn ident(s: &str) -> String {
    s.replace('-', "_")
}

pub fn version_suffix(v: Option<&semver::Version>) -> String {
    match v {
        Some(v) if v.major == 0 => format!("_v0_{}", v.minor),
        Some(v) => format!("_v{}", v.major),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub size: usize,
    pub align: usize,
}

impl Layout {
    pub const ZERO: Layout = Layout { size: 0, align: 1 };
    pub fn new(size: usize, align: usize) -> Self {
        Layout { size, align }
    }
}

pub fn align_to(n: usize, a: usize) -> usize {
    n.div_ceil(a) * a
}

/// Size of the discriminant for a sum type with `n` cases.
pub fn discriminant_size(n: usize) -> usize {
    match n {
        0..=256 => 1,
        257..=65536 => 2,
        _ => 4,
    }
}

pub fn flags_size(n: usize) -> usize {
    match n {
        0..=8 => 1,
        9..=16 => 2,
        _ => 4,
    }
}

/// A struct-like sequence of fields: offsets and overall layout.
#[derive(Clone, Debug)]
pub struct StructLayout {
    pub layout: Layout,
    pub offsets: Vec<usize>,
}

/// A sum type: discriminant, then the payload at `payload_offset`.
#[derive(Clone, Debug)]
pub struct SumLayout {
    pub layout: Layout,
    pub disc_size: usize,
    pub payload_offset: usize,
}

impl Wit {
    pub fn layout(&self, ty: Type) -> Layout {
        match ty {
            Type::Bool | Type::U8 | Type::S8 => Layout::new(1, 1),
            Type::U16 | Type::S16 => Layout::new(2, 2),
            Type::U32 | Type::S32 | Type::F32 | Type::Char => Layout::new(4, 4),
            Type::U64 | Type::S64 | Type::F64 => Layout::new(8, 8),
            Type::String => Layout::new(16, 8),
            Type::ErrorContext => Layout::new(4, 4),
            Type::Id(id) => self.layout_id(id),
        }
    }

    fn layout_id(&self, id: TypeId) -> Layout {
        match &self.resolve.types[id].kind {
            TypeDefKind::Type(t) => self.layout(*t),
            TypeDefKind::Record(r) => self.struct_layout(r.fields.iter().map(|f| f.ty)).layout,
            TypeDefKind::Tuple(t) => self.struct_layout(t.types.iter().copied()).layout,
            TypeDefKind::List(_) => Layout::new(16, 8),
            TypeDefKind::Handle(_) | TypeDefKind::Resource => Layout::new(4, 4),
            TypeDefKind::Flags(f) => {
                let s = flags_size(f.flags.len());
                Layout::new(s, s)
            }
            TypeDefKind::Enum(e) => {
                let s = discriminant_size(e.cases.len());
                Layout::new(s, s)
            }
            TypeDefKind::Variant(v) => self.sum_layout(v.cases.iter().map(|c| c.ty)).layout,
            TypeDefKind::Option(t) => self.sum_layout([None, Some(*t)].into_iter()).layout,
            TypeDefKind::Result(r) => self.sum_layout([r.ok, r.err].into_iter()).layout,
            k => panic!("no layout for {k:?}"),
        }
    }

    pub fn struct_layout(&self, fields: impl Iterator<Item = Type>) -> StructLayout {
        let mut off = 0;
        let mut align = 1;
        let mut offsets = Vec::new();
        for ty in fields {
            let l = self.layout(ty);
            off = align_to(off, l.align);
            offsets.push(off);
            off += l.size;
            align = align.max(l.align);
        }
        StructLayout { layout: Layout::new(align_to(off, align), align), offsets }
    }

    pub fn sum_layout(&self, cases: impl Iterator<Item = Option<Type>>) -> SumLayout {
        let cases: Vec<_> = cases.collect();
        let disc = discriminant_size(cases.len());
        let mut size = 0;
        let mut align = 1;
        for ty in cases.iter().flatten() {
            let l = self.layout(*ty);
            size = size.max(l.size);
            align = align.max(l.align);
        }
        let payload_offset = align_to(disc, align);
        let total_align = align.max(disc);
        SumLayout {
            layout: Layout::new(align_to(payload_offset + size, total_align), total_align),
            disc_size: disc,
            payload_offset,
        }
    }

    /// Layout of a function's argument struct (method `self` handle first).
    pub fn args_layout(&self, f: &Function) -> StructLayout {
        self.struct_layout(f.params.iter().map(|p| p.ty))
    }
}

// ---------------------------------------------------------------------------
// Table slots
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum SlotKind<'a> {
    /// A function, method, static or constructor.
    Func(&'a Function),
    /// `<resource>_drop(inst, args = &u32 handle, NULL, alloc)`.
    Drop(TypeId),
}

#[derive(Clone, Debug)]
pub struct Slot<'a> {
    pub kind: SlotKind<'a>,
    /// C field name: `get`, `counter_new`, `counter_add`, `counter_drop`.
    pub name: String,
    /// Version the slot was added in; `None` for unversioned packages.
    pub since: Option<Ver>,
    /// Byte offset of the function pointer in the table.
    pub offset: usize,
}

impl<'a> Slot<'a> {
    pub fn func(&self) -> Option<&'a Function> {
        match self.kind {
            SlotKind::Func(f) => Some(f),
            SlotKind::Drop(_) => None,
        }
    }
}

/// The function-table order of an interface: slots sorted by the version that
/// introduced them, then declaration order (functions before resource
/// drops). Later minors therefore only ever append.
pub fn slots<'a>(wit: &'a Wit, iface: InterfaceId) -> Vec<Slot<'a>> {
    let i = &wit.resolve.interfaces[iface];
    let base = wit.iface_version(iface).map(|v| Ver::new(v.major, if v.major == 0 { v.minor } else { 0 }, 0));
    let since = |s: &Stability| match s {
        Stability::Stable { since, .. } => Some(Ver::new(since.major, since.minor, since.patch)),
        _ => base.clone(),
    };
    let mut items: Vec<(Option<Ver>, usize, Slot<'a>)> = Vec::new();
    for (n, f) in i.functions.values().enumerate() {
        let mut v = since(&f.stability);
        // A resource's members are never older than the resource.
        if let Some(r) = resource_of_func(f) {
            v = v.max(since(&wit.resolve.types[r].stability));
        }
        items.push((v.clone(), n, Slot { kind: SlotKind::Func(f), name: func_c_name(&wit.resolve, f), since: v, offset: 0 }));
    }
    let nf = i.functions.len();
    for (n, (name, &ty)) in i.types.iter().enumerate() {
        if matches!(wit.resolve.types[ty].kind, TypeDefKind::Resource) {
            let v = since(&wit.resolve.types[ty].stability);
            items.push((v.clone(), nf + n, Slot { kind: SlotKind::Drop(ty), name: format!("{}_drop", ident(name)), since: v, offset: 0 }));
        }
    }
    items.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    items
        .into_iter()
        .enumerate()
        .map(|(n, (_, _, mut s))| {
            s.offset = TABLE_HEADER_SIZE + 8 * n;
            s
        })
        .collect()
}

/// Total byte size of an interface's table.
pub fn table_size(slots: &[Slot]) -> usize {
    TABLE_HEADER_SIZE + 8 * slots.len()
}

pub fn resource_of_func(f: &Function) -> Option<TypeId> {
    match f.kind {
        FunctionKind::Method(r) | FunctionKind::Static(r) | FunctionKind::Constructor(r) => Some(r),
        FunctionKind::AsyncMethod(r) | FunctionKind::AsyncStatic(r) => Some(r),
        _ => None,
    }
}

/// `get`, `counter_new`, `counter_add` (method or static).
pub fn func_c_name(resolve: &Resolve, f: &Function) -> String {
    match f.kind {
        FunctionKind::Constructor(r) => format!("{}_new", ident(resolve.types[r].name.as_ref().unwrap())),
        FunctionKind::Method(r) | FunctionKind::Static(r) => {
            format!("{}_{}", ident(resolve.types[r].name.as_ref().unwrap()), ident(f.item_name()))
        }
        _ => ident(&f.name),
    }
}

/// A plain, ordered version triple used for slot ordering.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Ver {
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Ver { major, minor, patch }
    }
}

impl std::fmt::Display for Ver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// ---------------------------------------------------------------------------
// Names shared by bindings
// ---------------------------------------------------------------------------

pub fn upper_camel(s: &str) -> String {
    s.to_upper_camel_case()
}

pub fn snake(s: &str) -> String {
    s.to_snake_case()
}

/// A stable name for an anonymous type, used by backends that need nominal
/// types for them (C): `list_u8`, `option_string`, `result_void_error`,
/// `tuple_f64_f64`.
pub fn mangle(wit: &Wit, ty: Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::U8 => "u8".into(),
        Type::U16 => "u16".into(),
        Type::U32 => "u32".into(),
        Type::U64 => "u64".into(),
        Type::S8 => "s8".into(),
        Type::S16 => "s16".into(),
        Type::S32 => "s32".into(),
        Type::S64 => "s64".into(),
        Type::F32 => "f32".into(),
        Type::F64 => "f64".into(),
        Type::Char => "char".into(),
        Type::String => "string".into(),
        Type::ErrorContext => "error_context".into(),
        Type::Id(id) => {
            let t = &wit.resolve.types[id];
            if let Some(n) = &t.name {
                return ident(n);
            }
            let opt = |t: Option<Type>| t.map(|t| mangle(wit, t)).unwrap_or_else(|| "void".into());
            match &t.kind {
                TypeDefKind::List(e) => format!("list_{}", mangle(wit, *e)),
                TypeDefKind::Option(e) => format!("option_{}", mangle(wit, *e)),
                TypeDefKind::Result(r) => format!("result_{}_{}", opt(r.ok), opt(r.err)),
                TypeDefKind::Tuple(tu) => {
                    let mut s = "tuple".to_string();
                    for t in &tu.types {
                        s.push('_');
                        s.push_str(&mangle(wit, *t));
                    }
                    s
                }
                TypeDefKind::Handle(Handle::Own(r)) => format!("own_{}", mangle(wit, Type::Id(*r))),
                TypeDefKind::Handle(Handle::Borrow(r)) => format!("borrow_{}", mangle(wit, Type::Id(*r))),
                TypeDefKind::Type(t) => mangle(wit, *t),
                k => panic!("cannot mangle {k:?}"),
            }
        }
    }
}

/// Every type reachable from an interface's functions and named types, in
/// dependency order (dependencies first), without duplicates.
pub fn reachable_types(wit: &Wit, iface: InterfaceId) -> Vec<TypeId> {
    let i = &wit.resolve.interfaces[iface];
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &t in i.types.values() {
        visit(wit, Type::Id(t), &mut seen, &mut out);
    }
    for f in i.functions.values() {
        for p in &f.params {
            visit(wit, p.ty, &mut seen, &mut out);
        }
        if let Some(r) = f.result {
            visit(wit, r, &mut seen, &mut out);
        }
    }
    out
}

fn visit(wit: &Wit, ty: Type, seen: &mut std::collections::HashSet<TypeId>, out: &mut Vec<TypeId>) {
    let Type::Id(id) = ty else { return };
    if !seen.insert(id) {
        return;
    }
    let mut deps: Vec<Type> = Vec::new();
    match &wit.resolve.types[id].kind {
        TypeDefKind::Record(r) => deps.extend(r.fields.iter().map(|f| f.ty)),
        TypeDefKind::Tuple(t) => deps.extend(t.types.iter().copied()),
        TypeDefKind::Variant(v) => deps.extend(v.cases.iter().filter_map(|c| c.ty)),
        TypeDefKind::Option(t) | TypeDefKind::List(t) | TypeDefKind::Type(t) => deps.push(*t),
        TypeDefKind::Result(r) => deps.extend(r.ok.iter().chain(r.err.iter()).copied()),
        TypeDefKind::Handle(h) => deps.push(Type::Id(match h {
            Handle::Own(r) | Handle::Borrow(r) => *r,
        })),
        _ => {}
    }
    for d in deps {
        visit(wit, d, seen, out);
    }
    out.push(id);
}

/// Interfaces whose types an interface uses (through `use`), excluding itself.
pub fn used_interfaces(wit: &Wit, iface: InterfaceId) -> Vec<InterfaceId> {
    let mut out = Vec::new();
    for id in reachable_types(wit, iface) {
        if let TypeOwner::Interface(o) = wit.resolve.types[id].owner {
            if o != iface && !out.contains(&o) {
                out.push(o);
            }
        }
    }
    out
}

/// Interface a named type belongs to.
pub fn owner_iface(wit: &Wit, id: TypeId) -> Option<InterfaceId> {
    match wit.resolve.types[id].owner {
        TypeOwner::Interface(i) => Some(i),
        _ => None,
    }
}

/// Packages in dependency order, for tools (wit-bindgen) that need the list.
pub fn package_dirs_in_order(root: &Path) -> Result<Vec<PathBuf>> {
    let wit = Wit::load(root)?;
    let mut by_name = BTreeMap::new();
    for d in std::fs::read_dir(root)? {
        let d = d?.path();
        if d.is_dir() && has_wit(&d)? {
            let g = UnresolvedPackageGroup::parse_dir(&d)?;
            by_name.insert(g.main.name.to_string(), d);
        }
    }
    let mut out = Vec::new();
    for (_, p) in wit.resolve.packages.iter() {
        if let Some(d) = by_name.get(&p.name.to_string()) {
            out.push(d.clone());
        }
    }
    Ok(out)
}
