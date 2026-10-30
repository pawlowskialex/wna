//! Kotlin/JVM (and Android) bindings over the generic JNI shim in
//! `tools/wna/kotlin`. Every slot has one C shape, so the output is pure
//! Kotlin: imports lower arguments into native memory at the model's offsets
//! and call `wna.WnaNative.call`; exports are dispatched to by the shim's
//! generic trampolines through `wna.Export`.
//!
//! Each interface `ns:pkg/name` becomes the Kotlin package `ns.pkg.name`:
//! * `<Name>Types.kt`: records (data classes), variants (sealed interfaces),
//!   enums, flags (value classes), `<E>Exception`s for error types, and the
//!   `<Name>Abi` object of lift/lower helpers.
//! * `<Name>.kt` when imported: `class <Name>(iface: wna.Iface, deps...)`
//!   plus an `AutoCloseable` class per resource.
//! * `exports/<Name>.kt` when exported: `interface <Name>` to implement, an
//!   interface per resource, and `class <Name>Export` (the table).

use crate::Files;
use anyhow::{Context, Result, bail};
use heck::{ToLowerCamelCase, ToShoutySnakeCase, ToUpperCamelCase};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use wna_abi::wit_parser::*;
use wna_abi::*;

#[derive(Clone, Default)]
pub struct Opts {
    /// Prefix for every generated Kotlin package (`com.acme` → `com.acme.ns.pkg.iface`).
    pub package: Option<String>,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
enum Dir {
    Import,
    Export,
}

/// Direction of each interface in a world.
type Dirs = HashMap<InterfaceId, Dir>;

/// Generates bindings for every interface the worlds import or export. One
/// interface may be imported by one world and exported by another (a test
/// driver and a server); both sides are emitted and share the type files.
pub fn generate(wit: &Wit, worlds: &[WorldId], opts: &Opts, files: &mut Files) -> Result<()> {
    if worlds.is_empty() {
        bail!("kotlin needs at least one --world");
    }
    let mut bindings: Vec<(InterfaceId, Dir, Dirs)> = Vec::new();
    let mut all: Vec<InterfaceId> = Vec::new();
    let mut seen_dirs: HashMap<InterfaceId, HashSet<Dir>> = HashMap::new();
    for &w in worlds {
        let (imports, exports) = wit.world_interfaces(w)?;
        let mut dirs = Dirs::new();
        for &i in &imports {
            dirs.insert(i, Dir::Import);
        }
        for &i in &exports {
            dirs.insert(i, Dir::Export);
        }
        for (&i, &d) in &dirs {
            wit.check_v0(i)?;
            seen_dirs.entry(i).or_default().insert(d);
        }
        let mut todo: Vec<_> = imports.iter().chain(&exports).copied().collect();
        while let Some(i) = todo.pop() {
            if !all.contains(&i) {
                all.push(i);
                todo.extend(used_interfaces(wit, i));
            }
        }
        let world_bindings = imports.iter().map(|&i| (i, Dir::Import)).chain(exports.iter().map(|&i| (i, Dir::Export)));
        for (i, d) in world_bindings {
            match bindings.iter().find(|b| b.0 == i && b.1 == d) {
                Some((_, _, prev)) => {
                    for o in used_interfaces(wit, i) {
                        if prev.get(&o) != dirs.get(&o) {
                            bail!("{} is bound differently by two worlds (its use of {}); generate them separately", wit.iface_id(i), wit.iface_id(o));
                        }
                    }
                }
                None => bindings.push((i, d, dirs.clone())),
            }
        }
    }
    // Type declarations are shared by both directions, so a named type that
    // holds a handle needs its resource's direction to agree across worlds.
    let global: Dirs = seen_dirs.iter().filter(|(_, d)| d.len() == 1).map(|(i, d)| (*i, *d.iter().next().unwrap())).collect();
    let mut err_types = HashSet::new();
    for &i in &all {
        for f in wit.resolve.interfaces[i].functions.values() {
            if let Some((_, Some(Type::Id(e)))) = top_result(wit, f) {
                let e = wit.dealias_id(e);
                if is_nominal(wit, e) {
                    err_types.insert(e);
                }
            }
        }
    }
    let g = Gen { wit, opts, global, err_types, fresh: Cell::new(0) };
    for &i in &all {
        let body = g.types_file(i)?;
        if !body.is_empty() {
            files.insert(format!("{}/{}Types.kt", g.pkg_path(i), g.iface_class(i)), body);
        }
    }
    for (i, d, dirs) in &bindings {
        if slots(wit, *i).is_empty() {
            continue;
        }
        match d {
            Dir::Import => {
                files.insert(format!("{}/{}.kt", g.pkg_path(*i), g.iface_class(*i)), g.import_file(*i, dirs)?);
            }
            Dir::Export => {
                files.insert(format!("{}/exports/{}.kt", g.pkg_path(*i), g.iface_class(*i)), g.export_file(*i, dirs)?);
            }
        }
    }
    Ok(())
}

/// `ok`/`err` of a function returning `result<..>`: these return `ok` and throw for `err`.
fn top_result(wit: &Wit, f: &Function) -> Option<(Option<Type>, Option<Type>)> {
    match wit.dealias(f.result?) {
        Type::Id(id) => match &wit.resolve.types[id].kind {
            TypeDefKind::Result(r) => Some((r.ok, r.err)),
            _ => None,
        },
        _ => None,
    }
}

/// Records, variants, enums and flags: types with a Kotlin declaration.
fn is_nominal(wit: &Wit, id: TypeId) -> bool {
    matches!(wit.resolve.types[id].kind, TypeDefKind::Record(_) | TypeDefKind::Variant(_) | TypeDefKind::Enum(_) | TypeDefKind::Flags(_))
}

const KEYWORDS: &[&str] = &[
    "as", "break", "class", "continue", "do", "else", "false", "for", "fun", "if", "in", "interface", "is", "null", "object",
    "package", "return", "super", "this", "throw", "true", "try", "typealias", "typeof", "val", "var", "when", "while",
];

/// `kebab-name` → `kebabName`, escaped if a keyword.
fn kt_ident(s: &str) -> String {
    let n = s.to_lower_camel_case();
    if KEYWORDS.contains(&n.as_str()) { format!("`{n}`") } else { n }
}

fn kt_class(s: &str) -> String {
    s.to_upper_camel_case()
}

fn pkg_seg(s: &str) -> String {
    let n = s.replace('-', "_").to_lowercase();
    if KEYWORDS.contains(&n.as_str()) { format!("{n}_") } else { n }
}

fn kdoc(d: &Docs, indent: &str) -> String {
    match &d.contents {
        Some(c) => {
            let mut s = format!("{indent}/**\n");
            for line in c.lines() {
                writeln!(s, "{indent} * {}", line.replace("*/", "*&#47;")).unwrap();
            }
            writeln!(s, "{indent} */").unwrap();
            s
        }
        None => String::new(),
    }
}

/// `o + n`, or `o` when `n` is 0.
fn at(o: &str, n: usize) -> String {
    if n == 0 { o.to_string() } else { format!("{o} + {n}") }
}

fn indent(n: usize) -> String {
    "    ".repeat(n)
}

const FILE_HEADER: &str = "@file:Suppress(\"unused\", \"UNUSED_PARAMETER\", \"UNUSED_VARIABLE\", \"UNCHECKED_CAST\", \"NAME_SHADOWING\", \"RedundantVisibilityModifier\", \"LocalVariableName\", \"FunctionName\", \"ClassName\", \"PropertyName\", \"RemoveRedundantQualifierName\")\n";

/// Where generated code runs: the interface being bound, the direction of
/// every interface in its world, and the expression of the binding object
/// (`this` in the import class or export table, `this.owner` in an imported
/// resource's class).
struct Ctx<'b> {
    iface: InterfaceId,
    dirs: &'b Dirs,
    base: &'b str,
}

struct Gen<'a> {
    wit: &'a Wit,
    opts: &'a Opts,
    global: Dirs,
    err_types: HashSet<TypeId>,
    fresh: Cell<usize>,
}

impl<'a> Gen<'a> {
    fn fresh(&self, p: &str) -> String {
        let n = self.fresh.get() + 1;
        self.fresh.set(n);
        format!("{p}_{n}")
    }

    fn pkg(&self, i: InterfaceId) -> String {
        let iface = &self.wit.resolve.interfaces[i];
        let p = &self.wit.resolve.packages[iface.package.unwrap()].name;
        let mut segs: Vec<String> = self.opts.package.iter().cloned().collect();
        segs.extend([&p.namespace, &p.name, iface.name.as_ref().unwrap()].map(|s| pkg_seg(s)));
        segs.join(".")
    }

    fn pkg_path(&self, i: InterfaceId) -> String {
        self.pkg(i).replace('.', "/")
    }

    fn iface_class(&self, i: InterfaceId) -> String {
        kt_class(self.wit.resolve.interfaces[i].name.as_ref().unwrap())
    }

    fn type_name(&self, id: TypeId) -> String {
        let t = &self.wit.resolve.types[id];
        format!("{}.{}", self.pkg(owner_iface(self.wit, id).unwrap()), kt_class(t.name.as_ref().unwrap()))
    }

    fn abi_obj(&self, i: InterfaceId) -> String {
        format!("{}.{}Abi", self.pkg(i), self.iface_class(i))
    }

    fn resource_name(&self, r: TypeId) -> String {
        kt_class(self.wit.resolve.types[r].name.as_ref().unwrap())
    }

    /// The class (imported) or interface (exported) standing for a resource.
    fn resource_type(&self, r: TypeId, dirs: &Dirs) -> Result<String> {
        let o = owner_iface(self.wit, r).unwrap();
        let name = self.resource_name(r);
        Ok(match self.dir_of(o, dirs)? {
            Dir::Import => format!("{}.{name}", self.pkg(o)),
            Dir::Export => format!("{}.exports.{name}", self.pkg(o)),
        })
    }

    fn dir_of(&self, o: InterfaceId, dirs: &Dirs) -> Result<Dir> {
        dirs.get(&o).copied().with_context(|| {
            format!("a resource of {} is used, but the world neither imports nor exports it (or worlds disagree on its direction)", self.wit.iface_id(o))
        })
    }

    fn is_option(&self, ty: Type) -> bool {
        matches!(self.wit.dealias(ty), Type::Id(id) if matches!(self.wit.resolve.types[id].kind, TypeDefKind::Option(_)))
    }

    fn is_bytes(&self, ty: Type) -> bool {
        matches!(self.wit.dealias(ty), Type::Id(id) if matches!(self.wit.resolve.types[id].kind, TypeDefKind::List(e) if self.wit.dealias(e) == Type::U8))
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
                _ => false,
            },
            _ => false,
        }
    }

    // -----------------------------------------------------------------------
    // Kotlin types

    fn kt(&self, ty: Type, dirs: &Dirs) -> Result<String> {
        Ok(match self.wit.dealias(ty) {
            Type::Bool => "kotlin.Boolean".into(),
            Type::U8 => "kotlin.UByte".into(),
            Type::U16 => "kotlin.UShort".into(),
            Type::U32 => "kotlin.UInt".into(),
            Type::U64 => "kotlin.ULong".into(),
            Type::S8 => "kotlin.Byte".into(),
            Type::S16 => "kotlin.Short".into(),
            Type::S32 => "kotlin.Int".into(),
            Type::S64 => "kotlin.Long".into(),
            Type::F32 => "kotlin.Float".into(),
            Type::F64 => "kotlin.Double".into(),
            Type::Char => "kotlin.Int".into(),
            Type::String => "kotlin.String".into(),
            Type::ErrorContext => bail!("error-context is not in WNA v0"),
            Type::Id(id) => match &self.wit.resolve.types[id].kind {
                TypeDefKind::Resource => self.resource_type(id, dirs)?,
                TypeDefKind::Handle(h) => self.resource_type(self.wit.resource_of(h), dirs)?,
                _ if is_nominal(self.wit, id) => self.type_name(id),
                TypeDefKind::List(_) if self.is_bytes(ty) => "kotlin.ByteArray".into(),
                TypeDefKind::List(e) => format!("kotlin.collections.List<{}>", self.kt(*e, dirs)?),
                TypeDefKind::Option(e) => {
                    let inner = self.kt(*e, dirs)?;
                    if self.is_option(*e) { format!("wna.Some<{inner}>?") } else { format!("{inner}?") }
                }
                TypeDefKind::Result(r) => format!("wna.WitResult<{}, {}>", self.kt_unit(r.ok, dirs)?, self.kt_unit(r.err, dirs)?),
                TypeDefKind::Tuple(t) => {
                    let items = t.types.iter().map(|t| self.kt(*t, dirs)).collect::<Result<Vec<_>>>()?;
                    format!("{}<{}>", tuple_class(items.len())?, items.join(", "))
                }
                k => bail!("unsupported type {k:?}"),
            },
        })
    }

    fn kt_unit(&self, ty: Option<Type>, dirs: &Dirs) -> Result<String> {
        match ty {
            Some(t) => self.kt(t, dirs),
            None => Ok("kotlin.Unit".into()),
        }
    }

    /// The binding object that owns resource handles of interface `o`.
    fn res_ctx(&self, cx: &Ctx, o: InterfaceId) -> String {
        if o == cx.iface { cx.base.to_string() } else { format!("{}.{}", cx.base, self.dep_name(o)) }
    }

    fn dep_name(&self, o: InterfaceId) -> String {
        kt_ident(self.wit.resolve.interfaces[o].name.as_ref().unwrap())
    }

    fn handles_prop(&self, r: TypeId) -> String {
        format!("{}Handles", kt_ident(self.wit.resolve.types[r].name.as_ref().unwrap()))
    }

    // -----------------------------------------------------------------------
    // Lifting: one Kotlin expression reading a value at `b[o]` (with `lx_`).

    fn lift(&self, ty: Type, b: &str, o: &str, cx: &Ctx) -> Result<String> {
        let ty = self.wit.dealias(ty);
        Ok(match ty {
            Type::Bool => format!("({b}.get({o}).toInt() != 0)"),
            Type::U8 => format!("{b}.get({o}).toUByte()"),
            Type::S8 => format!("{b}.get({o})"),
            Type::U16 => format!("{b}.getShort({o}).toUShort()"),
            Type::S16 => format!("{b}.getShort({o})"),
            Type::U32 => format!("{b}.getInt({o}).toUInt()"),
            Type::S32 => format!("{b}.getInt({o})"),
            Type::U64 => format!("{b}.getLong({o}).toULong()"),
            Type::S64 => format!("{b}.getLong({o})"),
            Type::F32 => format!("{b}.getFloat({o})"),
            Type::F64 => format!("{b}.getDouble({o})"),
            Type::Char => format!("wna.Lift.char({b}.getInt({o}))"),
            Type::String => format!("lx_.string({b}, {o})"),
            Type::ErrorContext => bail!("error-context is not in WNA v0"),
            Type::Id(id) if is_nominal(self.wit, id) && !self.has_handles(ty) => {
                format!("{}.lift{}({b}, {o}, lx_)", self.abi_obj(owner_iface(self.wit, id).unwrap()), kt_class(self.wit.resolve.types[id].name.as_ref().unwrap()))
            }
            Type::Id(id) => self.lift_body(id, b, o, cx)?,
        })
    }

    fn lift_body(&self, id: TypeId, b: &str, o: &str, cx: &Ctx) -> Result<String> {
        let t = &self.wit.resolve.types[id];
        let what = t.name.clone().unwrap_or_else(|| "value".into());
        Ok(match &t.kind {
            TypeDefKind::Record(r) => {
                let sl = self.wit.struct_layout(r.fields.iter().map(|f| f.ty));
                let mut args = Vec::new();
                for (f, off) in r.fields.iter().zip(&sl.offsets) {
                    args.push(format!("{} = {}", kt_ident(&f.name), self.lift(f.ty, b, &at(o, *off), cx)?));
                }
                format!("{}({})", self.type_name(id), args.join(", "))
            }
            TypeDefKind::Tuple(tu) => {
                let sl = self.wit.struct_layout(tu.types.iter().copied());
                let mut args = Vec::new();
                for (t, off) in tu.types.iter().zip(&sl.offsets) {
                    args.push(self.lift(*t, b, &at(o, *off), cx)?);
                }
                format!("{}({})", tuple_class(args.len())?, args.join(", "))
            }
            TypeDefKind::Variant(v) => {
                let sl = self.wit.sum_layout(v.cases.iter().map(|c| c.ty));
                let d = self.fresh("d");
                let name = self.type_name(id);
                let mut s = format!("when (val {d} = {}) {{\n", read_disc(b, o, sl.disc_size));
                for (n, c) in v.cases.iter().enumerate() {
                    match c.ty {
                        Some(ty) => writeln!(s, "{n} -> {name}.{}({})", kt_class(&c.name), self.lift(ty, b, &at(o, sl.payload_offset), cx)?).unwrap(),
                        None => writeln!(s, "{n} -> {name}.{}", kt_class(&c.name)).unwrap(),
                    }
                }
                write!(s, "else -> throw wna.WnaTrap.invalid(\"{what}: case \" + {d})\n}}").unwrap();
                s
            }
            TypeDefKind::Enum(e) => {
                format!("wna.Lift.case({}.entries, {}, \"{what}\")", self.type_name(id), read_disc(b, o, discriminant_size(e.cases.len())))
            }
            TypeDefKind::Flags(f) => format!("{}({})", self.type_name(id), read_disc(b, o, flags_size(f.flags.len()))),
            TypeDefKind::Option(ty) => {
                let sl = self.wit.sum_layout([None, Some(*ty)].into_iter());
                let mut inner = self.lift(*ty, b, &at(o, sl.payload_offset), cx)?;
                if self.is_option(*ty) {
                    inner = format!("wna.Some({inner})");
                }
                format!("(if (wna.Lift.bit({b}.get({o}), \"option\")) {inner} else null)")
            }
            TypeDefKind::Result(r) => {
                let sl = self.wit.sum_layout([r.ok, r.err].into_iter());
                let side = |t: Option<Type>| -> Result<String> {
                    match t {
                        Some(t) => self.lift(t, b, &at(o, sl.payload_offset), cx),
                        None => Ok("kotlin.Unit".into()),
                    }
                };
                format!("(if (wna.Lift.bit({b}.get({o}), \"result\")) wna.WitResult.Err({}) else wna.WitResult.Ok({}))", side(r.err)?, side(r.ok)?)
            }
            TypeDefKind::List(_) if self.is_bytes(Type::Id(id)) => format!("lx_.bytes({b}, {o})"),
            TypeDefKind::List(e) => {
                let (eb, eo) = (self.fresh("b"), self.fresh("o"));
                let size = self.wit.layout(*e).size;
                format!("lx_.list({b}, {o}, {size}) {{ {eb}, {eo} -> {} }}", self.lift(*e, &eb, &eo, cx)?)
            }
            TypeDefKind::Handle(h) => self.lift_handle(self.wit.resource_of(h), matches!(h, Handle::Own(_)), b, o, cx)?,
            TypeDefKind::Resource => self.lift_handle(id, true, b, o, cx)?,
            TypeDefKind::Type(t) => self.lift(*t, b, o, cx)?,
            k => bail!("unsupported type {k:?}"),
        })
    }

    fn lift_handle(&self, r: TypeId, own: bool, b: &str, o: &str, cx: &Ctx) -> Result<String> {
        let owner = owner_iface(self.wit, r).unwrap();
        let c = self.res_ctx(cx, owner);
        let raw = format!("{b}.getInt({o})");
        Ok(match (self.dir_of(owner, cx.dirs)?, own) {
            (Dir::Import, true) => format!("{}.fromHandle({c}, {raw}.toUInt())", self.resource_type(r, cx.dirs)?),
            (Dir::Import, false) => format!("{}.borrow({c}, {raw}.toUInt())", self.resource_type(r, cx.dirs)?),
            (Dir::Export, true) => format!("{c}.{}.remove({raw})", self.handles_prop(r)),
            (Dir::Export, false) => format!("{c}.{}.get({raw})", self.handles_prop(r)),
        })
    }

    // -----------------------------------------------------------------------
    // Lowering: statements writing `v` at `b[o]` (allocating with `cx_`).

    #[allow(clippy::too_many_arguments)]
    fn lower(&self, ty: Type, v: &str, b: &str, o: &str, cx: &Ctx, out: &mut String, ind: usize) -> Result<()> {
        let ty = self.wit.dealias(ty);
        let pad = indent(ind);
        let line = match ty {
            Type::Bool => format!("{b}.put({o}, (if ({v}) 1 else 0).toByte())"),
            Type::U8 => format!("{b}.put({o}, {v}.toByte())"),
            Type::S8 => format!("{b}.put({o}, {v})"),
            Type::U16 => format!("{b}.putShort({o}, {v}.toShort())"),
            Type::S16 => format!("{b}.putShort({o}, {v})"),
            Type::U32 => format!("{b}.putInt({o}, {v}.toInt())"),
            Type::S32 => format!("{b}.putInt({o}, {v})"),
            Type::U64 => format!("{b}.putLong({o}, {v}.toLong())"),
            Type::S64 => format!("{b}.putLong({o}, {v})"),
            Type::F32 => format!("{b}.putFloat({o}, {v})"),
            Type::F64 => format!("{b}.putDouble({o}, {v})"),
            Type::Char => format!("{b}.putInt({o}, wna.Lower.char({v}))"),
            Type::String => format!("cx_.string({v}, {b}, {o})"),
            Type::ErrorContext => bail!("error-context is not in WNA v0"),
            Type::Id(id) if is_nominal(self.wit, id) && !self.has_handles(ty) => {
                format!("{}.lower{}({v}, {b}, {o}, cx_)", self.abi_obj(owner_iface(self.wit, id).unwrap()), kt_class(self.wit.resolve.types[id].name.as_ref().unwrap()))
            }
            Type::Id(id) => return self.lower_body(id, v, b, o, cx, out, ind),
        };
        writeln!(out, "{pad}{line}").unwrap();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_body(&self, id: TypeId, v: &str, b: &str, o: &str, cx: &Ctx, out: &mut String, ind: usize) -> Result<()> {
        let t = &self.wit.resolve.types[id];
        let pad = indent(ind);
        match &t.kind {
            TypeDefKind::Record(r) => {
                let sl = self.wit.struct_layout(r.fields.iter().map(|f| f.ty));
                for (f, off) in r.fields.iter().zip(&sl.offsets) {
                    self.lower(f.ty, &format!("{v}.{}", kt_ident(&f.name)), b, &at(o, *off), cx, out, ind)?;
                }
            }
            TypeDefKind::Tuple(tu) => {
                let sl = self.wit.struct_layout(tu.types.iter().copied());
                for (n, (t, off)) in tu.types.iter().zip(&sl.offsets).enumerate() {
                    self.lower(*t, &format!("{v}.component{}()", n + 1), b, &at(o, *off), cx, out, ind)?;
                }
            }
            TypeDefKind::Variant(var) => {
                let sl = self.wit.sum_layout(var.cases.iter().map(|c| c.ty));
                let x = self.fresh("v");
                let name = self.type_name(id);
                writeln!(out, "{pad}when (val {x} = {v}) {{").unwrap();
                for (n, c) in var.cases.iter().enumerate() {
                    writeln!(out, "{pad}    is {name}.{} -> {{", kt_class(&c.name)).unwrap();
                    writeln!(out, "{pad}        {}", write_disc(b, o, sl.disc_size, &n.to_string())).unwrap();
                    if let Some(ty) = c.ty {
                        self.lower(ty, &format!("{x}.value"), b, &at(o, sl.payload_offset), cx, out, ind + 2)?;
                    }
                    writeln!(out, "{pad}    }}").unwrap();
                }
                writeln!(out, "{pad}}}").unwrap();
            }
            TypeDefKind::Enum(e) => writeln!(out, "{pad}{}", write_disc(b, o, discriminant_size(e.cases.len()), &format!("{v}.ordinal"))).unwrap(),
            TypeDefKind::Flags(f) => writeln!(out, "{pad}{}", write_disc(b, o, flags_size(f.flags.len()), &format!("{v}.bits"))).unwrap(),
            TypeDefKind::Option(ty) => {
                let sl = self.wit.sum_layout([None, Some(*ty)].into_iter());
                let x = self.fresh("v");
                writeln!(out, "{pad}val {x} = {v}").unwrap();
                writeln!(out, "{pad}if ({x} == null) {{\n{pad}    {b}.put({o}, 0.toByte())\n{pad}}} else {{\n{pad}    {b}.put({o}, 1.toByte())").unwrap();
                let inner = if self.is_option(*ty) { format!("{x}.value") } else { x.clone() };
                self.lower(*ty, &inner, b, &at(o, sl.payload_offset), cx, out, ind + 1)?;
                writeln!(out, "{pad}}}").unwrap();
            }
            TypeDefKind::Result(r) => {
                let sl = self.wit.sum_layout([r.ok, r.err].into_iter());
                let x = self.fresh("v");
                writeln!(out, "{pad}when (val {x} = {v}) {{").unwrap();
                for (n, (case, ty)) in [("Ok", r.ok), ("Err", r.err)].into_iter().enumerate() {
                    writeln!(out, "{pad}    is wna.WitResult.{case} -> {{\n{pad}        {b}.put({o}, {n}.toByte())").unwrap();
                    if let Some(ty) = ty {
                        self.lower(ty, &format!("{x}.value"), b, &at(o, sl.payload_offset), cx, out, ind + 2)?;
                    }
                    writeln!(out, "{pad}    }}").unwrap();
                }
                writeln!(out, "{pad}}}").unwrap();
            }
            TypeDefKind::List(_) if self.is_bytes(Type::Id(id)) => writeln!(out, "{pad}cx_.bytes({v}, {b}, {o})").unwrap(),
            TypeDefKind::List(e) => {
                let l = self.wit.layout(*e);
                let (x, eb, eo) = (self.fresh("e"), self.fresh("b"), self.fresh("o"));
                writeln!(out, "{pad}cx_.list({v}, {}, {}, {b}, {o}) {{ {x}, {eb}, {eo} ->", l.size, l.align).unwrap();
                self.lower(*e, &x, &eb, &eo, cx, out, ind + 1)?;
                writeln!(out, "{pad}}}").unwrap();
            }
            TypeDefKind::Handle(h) => self.lower_handle(self.wit.resource_of(h), matches!(h, Handle::Own(_)), v, b, o, cx, out, ind)?,
            TypeDefKind::Resource => self.lower_handle(id, true, v, b, o, cx, out, ind)?,
            TypeDefKind::Type(t) => self.lower(*t, v, b, o, cx, out, ind)?,
            k => bail!("unsupported type {k:?}"),
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_handle(&self, r: TypeId, own: bool, v: &str, b: &str, o: &str, cx: &Ctx, out: &mut String, ind: usize) -> Result<()> {
        let owner = owner_iface(self.wit, r).unwrap();
        let h = match (self.dir_of(owner, cx.dirs)?, own) {
            // Passing an owned handle gives it away.
            (Dir::Import, true) => format!("{v}.takeHandle().toInt()"),
            (Dir::Import, false) => format!("{v}.handle.toInt()"),
            (Dir::Export, true) => format!("{}.{}.insert({v})", self.res_ctx(cx, owner), self.handles_prop(r)),
            (Dir::Export, false) => bail!("passing a borrow of a resource this side exports is not supported by the Kotlin backend yet"),
        };
        writeln!(out, "{}{b}.putInt({o}, {h})", indent(ind)).unwrap();
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Types file

    fn types_file(&self, iface: InterfaceId) -> Result<String> {
        let i = &self.wit.resolve.interfaces[iface];
        let cx = Ctx { iface, dirs: &self.global, base: "this" };
        let mut decls = String::new();
        let mut helpers = String::new();
        for (name, &id) in &i.types {
            let t = &self.wit.resolve.types[id];
            let cname = kt_class(name);
            let fq = self.type_name(id);
            decls.push_str(&kdoc(&t.docs, ""));
            match &t.kind {
                TypeDefKind::Record(r) => {
                    let mut fields = Vec::new();
                    for f in &r.fields {
                        fields.push(format!("{}    public val {}: {},\n", kdoc(&f.docs, "    "), kt_ident(&f.name), self.kt(f.ty, &self.global)?));
                    }
                    if fields.is_empty() {
                        writeln!(decls, "public class {cname} {{\n    override fun equals(other: kotlin.Any?): kotlin.Boolean = other is {cname}\n    override fun hashCode(): kotlin.Int = 0\n    override fun toString(): kotlin.String = \"{cname}()\"\n}}").unwrap();
                    } else {
                        let arrays: Vec<(String, bool)> = r.fields.iter().map(|f| (kt_ident(&f.name), self.is_bytes(f.ty))).collect();
                        writeln!(decls, "public data class {cname}(\n{}){}", fields.concat(), array_equality(&cname, &arrays, "    ")).unwrap();
                    }
                }
                TypeDefKind::Variant(v) => {
                    writeln!(decls, "public sealed interface {cname} {{").unwrap();
                    for c in &v.cases {
                        decls.push_str(&kdoc(&c.docs, "    "));
                        match c.ty {
                            Some(ty) => {
                                let case = kt_class(&c.name);
                                let eq = array_equality(&format!("{cname}.{case}"), &[("value".into(), self.is_bytes(ty))], "        ");
                                writeln!(decls, "    public data class {case}(public val value: {}) : {cname}{eq}", self.kt(ty, &self.global)?).unwrap();
                            }
                            None => writeln!(decls, "    public data object {} : {cname}", kt_class(&c.name)).unwrap(),
                        }
                    }
                    writeln!(decls, "}}").unwrap();
                }
                TypeDefKind::Enum(e) => {
                    writeln!(decls, "public enum class {cname} {{").unwrap();
                    for c in &e.cases {
                        decls.push_str(&kdoc(&c.docs, "    "));
                        writeln!(decls, "    {},", c.name.to_shouty_snake_case()).unwrap();
                    }
                    writeln!(decls, "}}").unwrap();
                }
                TypeDefKind::Flags(f) => {
                    let all: u64 = if f.flags.len() == 32 { u32::MAX as u64 } else { (1u64 << f.flags.len()) - 1 };
                    writeln!(decls, "@JvmInline\npublic value class {cname}(public val bits: kotlin.Int) {{").unwrap();
                    writeln!(decls, "    public infix fun or(other: {cname}): {cname} = {cname}(bits or other.bits)").unwrap();
                    writeln!(decls, "    public infix fun and(other: {cname}): {cname} = {cname}(bits and other.bits)").unwrap();
                    writeln!(decls, "    public operator fun contains(other: {cname}): kotlin.Boolean = (bits and other.bits) == other.bits").unwrap();
                    let names: Vec<String> = f.flags.iter().map(|fl| format!("\"{}\"", fl.name)).collect();
                    writeln!(
                        decls,
                        "    override fun toString(): kotlin.String = \"{cname}(\" + kotlin.collections.listOf({}).filterIndexed {{ i, _ -> (bits ushr i) and 1 != 0 }}.joinToString(\" | \") + \")\"",
                        names.join(", ")
                    )
                    .unwrap();
                    writeln!(decls, "    public companion object {{").unwrap();
                    writeln!(decls, "        public val NONE: {cname} = {cname}(0)").unwrap();
                    writeln!(decls, "        public val ALL: {cname} = {cname}({})", all as u32 as i32).unwrap();
                    for (n, fl) in f.flags.iter().enumerate() {
                        decls.push_str(&kdoc(&fl.docs, "        "));
                        writeln!(decls, "        public val {}: {cname} = {cname}(1 shl {n})", fl.name.to_shouty_snake_case()).unwrap();
                    }
                    writeln!(decls, "    }}\n}}").unwrap();
                }
                TypeDefKind::Type(target) => {
                    // `use` brings types of other interfaces into scope: refer to those directly.
                    let foreign = matches!(target, Type::Id(t) if owner_iface(self.wit, *t) != Some(iface));
                    if !foreign && !self.has_handles(*target) {
                        writeln!(decls, "public typealias {cname} = {}", self.kt(*target, &self.global)?).unwrap();
                    }
                }
                TypeDefKind::Resource => {}
                k => bail!("unsupported named type {name}: {k:?}"),
            }
            if self.err_types.contains(&id) {
                writeln!(decls, "/** The `err` of a function returning `result<_, {name}>`. */\npublic class {cname}Exception(override val payload: {fq}) : wna.WitError(payload)").unwrap();
            }
            if is_nominal(self.wit, id) && !self.has_handles(Type::Id(id)) {
                writeln!(helpers, "    @JvmStatic public fun lift{cname}(b: java.nio.ByteBuffer, o: kotlin.Int, lx_: wna.Lift): {fq} =\n        {}", self.lift_body(id, "b", "o", &cx)?).unwrap();
                let mut body = String::new();
                self.lower_body(id, "v", "b", "o", &cx, &mut body, 2)?;
                writeln!(helpers, "    @JvmStatic public fun lower{cname}(v: {fq}, b: java.nio.ByteBuffer, o: kotlin.Int, cx_: wna.Lower) {{\n{body}    }}").unwrap();
            }
        }
        if decls.is_empty() && helpers.is_empty() {
            return Ok(String::new());
        }
        let mut s = self.header(iface, &self.pkg(iface));
        s.push_str(&decls);
        if !helpers.is_empty() {
            writeln!(s, "\n/** Memory layout of this interface's types. Used by generated code. */\npublic object {}Abi {{\n{helpers}}}", self.iface_class(iface)).unwrap();
        }
        Ok(s)
    }

    fn header(&self, iface: InterfaceId, pkg: &str) -> String {
        format!("// Generated by wna-gen from {}. Do not edit.\n{FILE_HEADER}\npackage {pkg}\n\n", self.wit.iface_id(iface))
    }

    // -----------------------------------------------------------------------
    // Shared by both directions

    /// Other interfaces whose binding objects this binding needs: owners of
    /// handles it lifts that it imports, and of every handle it exports.
    fn deps(&self, iface: InterfaceId, dir: Dir, dirs: &Dirs) -> Result<Vec<(InterfaceId, Dir)>> {
        let mut out = Vec::new();
        for f in self.wit.resolve.interfaces[iface].functions.values() {
            for p in &f.params {
                self.walk_deps(iface, p.ty, dir == Dir::Export, dirs, &mut out)?;
            }
            if let Some(r) = f.result {
                self.walk_deps(iface, r, dir == Dir::Import, dirs, &mut out)?;
            }
        }
        Ok(out)
    }

    fn walk_deps(&self, iface: InterfaceId, ty: Type, lifting: bool, dirs: &Dirs, out: &mut Vec<(InterfaceId, Dir)>) -> Result<()> {
        let Type::Id(id) = self.wit.dealias(ty) else { return Ok(()) };
        let mut sub: Vec<Type> = Vec::new();
        match &self.wit.resolve.types[id].kind {
            TypeDefKind::Record(r) => sub.extend(r.fields.iter().map(|f| f.ty)),
            TypeDefKind::Tuple(t) => sub.extend(t.types.iter().copied()),
            TypeDefKind::Variant(v) => sub.extend(v.cases.iter().filter_map(|c| c.ty)),
            TypeDefKind::Option(t) | TypeDefKind::List(t) => sub.push(*t),
            TypeDefKind::Result(r) => sub.extend(r.ok.iter().chain(r.err.iter()).copied()),
            TypeDefKind::Handle(_) | TypeDefKind::Resource => {
                let r = match &self.wit.resolve.types[id].kind {
                    TypeDefKind::Handle(h) => self.wit.resource_of(h),
                    _ => id,
                };
                let o = owner_iface(self.wit, r).unwrap();
                let d = self.dir_of(o, dirs)?;
                if o != iface && (lifting || d == Dir::Export) && !out.contains(&(o, d)) {
                    out.push((o, d));
                }
            }
            _ => {}
        }
        for t in sub {
            self.walk_deps(iface, t, lifting, dirs, out)?;
        }
        Ok(())
    }

    fn dep_params(&self, deps: &[(InterfaceId, Dir)]) -> Vec<String> {
        deps.iter()
            .map(|&(o, d)| match d {
                Dir::Import => format!("public val {}: {}.{}", self.dep_name(o), self.pkg(o), self.iface_class(o)),
                Dir::Export => format!("public val {}: {}.exports.{}Export", self.dep_name(o), self.pkg(o), self.iface_class(o)),
            })
            .collect()
    }

    /// Kotlin name of a function: free `lowerCamel`, constructor `new<Resource>`,
    /// static `<resource><Name>`, method `<name>` (on the resource).
    fn fn_name(&self, f: &Function) -> String {
        match f.kind {
            FunctionKind::Constructor(r) => format!("new{}", self.resource_name(r)),
            FunctionKind::Static(r) => kt_ident(&format!("{}-{}", self.wit.resolve.types[r].name.as_ref().unwrap(), f.item_name())),
            FunctionKind::Method(_) => kt_ident(f.item_name()),
            _ => kt_ident(&f.name),
        }
    }

    /// The Kotlin return type: a top-level `result<T, E>` returns `T` and throws for `E`.
    fn ret_type(&self, f: &Function, dirs: &Dirs) -> Result<String> {
        match (top_result(self.wit, f), f.result) {
            (Some((ok, _)), _) => self.kt_unit(ok, dirs),
            (None, Some(r)) => self.kt(r, dirs),
            (None, None) => Ok("kotlin.Unit".into()),
        }
    }

    /// The exception class for a top-level result's `err` and how to build/read its payload.
    fn err_class(&self, err: Option<Type>) -> String {
        match err.map(|t| self.wit.dealias(t)) {
            Some(Type::Id(e)) if self.err_types.contains(&e) => format!("{}Exception", self.type_name(e)),
            _ => "wna.WitError".into(),
        }
    }

    fn params(&self, f: &Function, dirs: &Dirs) -> Result<Vec<String>> {
        let skip = matches!(f.kind, FunctionKind::Method(_)) as usize;
        f.params.iter().skip(skip).map(|p| Ok(format!("{}: {}", kt_ident(&p.name), self.kt(p.ty, dirs)?))).collect()
    }

    fn what(&self, iface: InterfaceId, f: &Function) -> String {
        format!("{}.{}", self.wit.iface_name(iface).unwrap(), f.name)
    }

    // -----------------------------------------------------------------------
    // Imports

    fn import_file(&self, iface: InterfaceId, dirs: &Dirs) -> Result<String> {
        let i = &self.wit.resolve.interfaces[iface];
        let class = self.iface_class(iface);
        let deps = self.deps(iface, Dir::Import, dirs)?;
        let slots = slots(self.wit, iface);
        let mut s = self.header(iface, &self.pkg(iface));
        s.push_str(&kdoc(&i.docs, ""));
        let mut ctor = vec!["public val iface: wna.Iface".to_string()];
        ctor.extend(self.dep_params(&deps));
        writeln!(s, "public class {class}({}) {{", ctor.join(", ")).unwrap();
        let cx = Ctx { iface, dirs, base: "this" };
        for slot in &slots {
            if let Some(f) = slot.func()
                && !matches!(f.kind, FunctionKind::Method(_))
            {
                s.push_str(&kdoc(&f.docs, "    "));
                self.import_fn(iface, slot, f, &cx, "this.iface", &mut s)?;
            }
        }
        writeln!(s, "}}").unwrap();

        for (name, &r) in &i.types {
            if !matches!(self.wit.resolve.types[r].kind, TypeDefKind::Resource) {
                continue;
            }
            let rname = kt_class(name);
            let drop = slots.iter().find(|sl| matches!(sl.kind, SlotKind::Drop(d) if d == r)).unwrap().offset;
            s.push_str(&kdoc(&self.wit.resolve.types[r].docs, ""));
            writeln!(
                s,
                "public class {rname} private constructor(public val owner: {class}, handle: kotlin.UInt, private val owned: kotlin.Boolean) : java.lang.AutoCloseable {{
    private val h = java.util.concurrent.atomic.AtomicInteger(handle.toInt())

    /** The raw handle (0 once closed or moved). */
    public val handle: kotlin.UInt get() = h.get().toUInt()

    /** Gives up ownership: `close` will not drop the handle. */
    public fun takeHandle(): kotlin.UInt = h.getAndSet(0).toUInt()

    /** Drops the resource (for an owned handle). */
    override fun close() {{
        val x = takeHandle()
        if (x != 0u && owned) this.owner.iface.drop({drop}, x.toInt())
    }}

    override fun toString(): kotlin.String = \"{rname}(\" + handle + \")\"

    public companion object {{
        /** Adopts an owned handle. */
        @JvmStatic public fun fromHandle(owner: {class}, handle: kotlin.UInt): {rname} = {rname}(owner, handle, true)

        /** Wraps a borrowed handle; `close` does not drop it. */
        @JvmStatic public fun borrow(owner: {class}, handle: kotlin.UInt): {rname} = {rname}(owner, handle, false)
    }}
"
            )
            .unwrap();
            let rcx = Ctx { iface, dirs, base: "this.owner" };
            for slot in &slots {
                if let Some(f) = slot.func()
                    && matches!(f.kind, FunctionKind::Method(x) if x == r)
                {
                    s.push_str(&kdoc(&f.docs, "    "));
                    self.import_fn(iface, slot, f, &rcx, "this.owner.iface", &mut s)?;
                }
            }
            writeln!(s, "}}").unwrap();
        }
        Ok(s)
    }

    fn import_fn(&self, iface: InterfaceId, slot: &Slot, f: &Function, cx: &Ctx, iface_expr: &str, s: &mut String) -> Result<()> {
        let is_method = matches!(f.kind, FunctionKind::Method(_));
        let ret = self.ret_type(f, cx.dirs)?;
        writeln!(s, "    public fun {}({}): {ret} {{", self.fn_name(f), self.params(f, cx.dirs)?.join(", ")).unwrap();
        writeln!(s, "        val cx_ = wna.Arena.acquire()\n        try {{").unwrap();
        let args = if f.params.is_empty() {
            "0L".to_string()
        } else {
            let sl = self.wit.args_layout(f);
            writeln!(s, "            val a_ = cx_.block({}, {})", sl.layout.size, sl.layout.align).unwrap();
            for (n, (p, off)) in f.params.iter().zip(&sl.offsets).enumerate() {
                let o = at("a_.off", *off);
                if is_method && n == 0 {
                    writeln!(s, "            a_.buf.putInt({o}, this.handle.toInt())").unwrap();
                } else {
                    self.lower(p.ty, &kt_ident(&p.name), "a_.buf", &o, cx, s, 3)?;
                }
            }
            "a_.addr".to_string()
        };
        let what = self.what(iface, f);
        match f.result {
            None => writeln!(s, "            {iface_expr}.call({}, {args}, 0L, \"{what}\")", slot.offset).unwrap(),
            Some(r) => {
                let l = self.wit.layout(r);
                writeln!(s, "            val r_ = cx_.block({}, {})", l.size, l.align).unwrap();
                writeln!(s, "            {iface_expr}.call({}, {args}, r_.addr, \"{what}\")", slot.offset).unwrap();
                let lifted = format!("wna.Lift.result {{ lx_ -> {} }}", self.lift(r, "r_.buf", "r_.off", cx)?);
                match top_result(self.wit, f) {
                    None => writeln!(s, "            return {lifted}").unwrap(),
                    Some((_, err)) => {
                        let exc = self.err_class(err);
                        let payload = if err.is_some() { "v_.value" } else { "null" };
                        writeln!(
                            s,
                            "            val v_: {} = {lifted}\n            return when (v_) {{\n                is wna.WitResult.Ok -> v_.value\n                is wna.WitResult.Err -> throw {exc}({payload})\n            }}",
                            self.kt(r, cx.dirs)?
                        )
                        .unwrap();
                    }
                }
            }
        }
        writeln!(s, "        }} finally {{\n            cx_.release()\n        }}\n    }}\n").unwrap();
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Exports

    fn export_file(&self, iface: InterfaceId, dirs: &Dirs) -> Result<String> {
        let i = &self.wit.resolve.interfaces[iface];
        let class = self.iface_class(iface);
        let slots = slots(self.wit, iface);
        if slots.len() > 256 {
            bail!("{}: the Kotlin backend exports at most 256 slots per interface", self.wit.iface_id(iface));
        }
        let deps = self.deps(iface, Dir::Export, dirs)?;
        let resources: Vec<(TypeId, String)> =
            i.types.iter().filter(|(_, t)| matches!(self.wit.resolve.types[**t].kind, TypeDefKind::Resource)).map(|(n, t)| (*t, kt_class(n))).collect();
        let mut s = self.header(iface, &format!("{}.exports", self.pkg(iface)));

        // What the implementation provides.
        s.push_str(&kdoc(&i.docs, ""));
        writeln!(s, "public interface {class} {{").unwrap();
        for slot in &slots {
            if let Some(f) = slot.func()
                && !matches!(f.kind, FunctionKind::Method(_))
            {
                s.push_str(&kdoc(&f.docs, "    "));
                writeln!(s, "    public fun {}({}): {}\n", self.fn_name(f), self.params(f, dirs)?.join(", "), self.ret_type(f, dirs)?).unwrap();
            }
        }
        writeln!(s, "}}\n").unwrap();
        for (r, rname) in &resources {
            s.push_str(&kdoc(&self.wit.resolve.types[*r].docs, ""));
            writeln!(s, "public interface {rname} : java.lang.AutoCloseable {{").unwrap();
            for slot in &slots {
                if let Some(f) = slot.func()
                    && matches!(f.kind, FunctionKind::Method(x) if x == *r)
                {
                    s.push_str(&kdoc(&f.docs, "    "));
                    writeln!(s, "    public fun {}({}): {}\n", self.fn_name(f), self.params(f, dirs)?.join(", "), self.ret_type(f, dirs)?).unwrap();
                }
            }
            writeln!(s, "    /** Called when the last handle is dropped. */\n    override fun close() {{}}\n}}\n").unwrap();
        }

        // The table.
        let mut ctor = vec![format!("public val impl: {class}")];
        ctor.extend(self.dep_params(&deps));
        writeln!(s, "/** Exposes an implementation of `{}` as a WNA table: pass [iface] to native code. */", self.wit.iface_id(iface)).unwrap();
        writeln!(s, "public class {class}Export({}) : wna.Export({}) {{", ctor.join(", "), slots.len()).unwrap();
        for (r, rname) in &resources {
            writeln!(s, "    public val {}: wna.HandleTable<{rname}> = wna.HandleTable()", self.handles_prop(*r)).unwrap();
        }
        writeln!(s, "\n    override fun call(slot: kotlin.Int, args: kotlin.Long, ret: kotlin.Long, alloc: kotlin.Long) {{\n        when (slot) {{").unwrap();
        for (n, slot) in slots.iter().enumerate() {
            writeln!(s, "            {n} -> s_{}(args, ret, alloc)", slot.name).unwrap();
        }
        writeln!(s, "            else -> throw wna.WnaTrap(wna.WnaTrap.UNAVAILABLE, \"no slot \" + slot)\n        }}\n    }}\n").unwrap();
        let cx = Ctx { iface, dirs, base: "this" };
        for slot in &slots {
            writeln!(s, "    private fun s_{}(args_: kotlin.Long, ret_: kotlin.Long, alloc_: kotlin.Long) {{", slot.name).unwrap();
            match slot.kind {
                SlotKind::Drop(r) => {
                    writeln!(s, "        this.{}.remove(wna.Lift.view(args_, 4).getInt(0)).close()", self.handles_prop(r)).unwrap();
                }
                SlotKind::Func(f) => self.export_body(f, &cx, &mut s)?,
            }
            writeln!(s, "    }}\n").unwrap();
        }
        writeln!(s, "}}").unwrap();
        Ok(s)
    }

    fn export_body(&self, f: &Function, cx: &Ctx, s: &mut String) -> Result<()> {
        let mut call_args = Vec::new();
        let mut recv = "this.impl".to_string();
        if !f.params.is_empty() {
            let sl = self.wit.args_layout(f);
            writeln!(s, "        val a_ = wna.Lift.view(args_, {})\n        val lx_ = wna.Lift(false)", sl.layout.size).unwrap();
            for (n, (p, off)) in f.params.iter().zip(&sl.offsets).enumerate() {
                if n == 0 && matches!(f.kind, FunctionKind::Method(_)) {
                    let FunctionKind::Method(r) = f.kind else { unreachable!() };
                    writeln!(s, "        val self_ = this.{}.get(a_.getInt({off}))", self.handles_prop(r)).unwrap();
                    recv = "self_".into();
                    continue;
                }
                let name = format!("p_{}", p.name.to_lower_camel_case());
                writeln!(s, "        val {name} = {}", self.lift(p.ty, "a_", &off.to_string(), cx)?).unwrap();
                call_args.push(name);
            }
        }
        let call = format!("{recv}.{}({})", self.fn_name(f), call_args.join(", "));
        let Some(r) = f.result else {
            writeln!(s, "        {call}").unwrap();
            return Ok(());
        };
        match top_result(self.wit, f) {
            None => writeln!(s, "        val v_ = {call}").unwrap(),
            Some((_, err)) => {
                let exc = self.err_class(err);
                let payload = match err {
                    None => "kotlin.Unit".to_string(),
                    Some(e) if exc == "wna.WitError" => format!("e_.payload as {}", self.kt(e, cx.dirs)?),
                    Some(_) => "e_.payload".into(),
                };
                writeln!(
                    s,
                    "        val v_: {} = try {{\n            wna.WitResult.Ok({call})\n        }} catch (e_: {exc}) {{\n            wna.WitResult.Err({payload})\n        }}",
                    self.kt(r, cx.dirs)?
                )
                .unwrap();
            }
        }
        let l = self.wit.layout(r);
        writeln!(s, "        val cx_ = wna.ResultLower(alloc_)\n        try {{\n            val b_ = wna.Lift.view(ret_, {})", l.size).unwrap();
        self.lower(r, "v_", "b_", "0", cx, s, 3)?;
        writeln!(s, "        }} catch (t_: kotlin.Throwable) {{\n            cx_.rollback()\n            throw t_\n        }}").unwrap();
        Ok(())
    }
}

fn tuple_class(n: usize) -> Result<&'static str> {
    Ok(match n {
        1 => "wna.Tuple1",
        2 => "kotlin.Pair",
        3 => "kotlin.Triple",
        4 => "wna.Tuple4",
        5 => "wna.Tuple5",
        6 => "wna.Tuple6",
        7 => "wna.Tuple7",
        8 => "wna.Tuple8",
        _ => bail!("tuples of {n} elements are not supported by the Kotlin backend"),
    })
}

fn read_disc(b: &str, o: &str, size: usize) -> String {
    match size {
        1 => format!("({b}.get({o}).toInt() and 0xff)"),
        2 => format!("({b}.getShort({o}).toInt() and 0xffff)"),
        _ => format!("{b}.getInt({o})"),
    }
}

fn write_disc(b: &str, o: &str, size: usize, v: &str) -> String {
    match size {
        1 => format!("{b}.put({o}, ({v}).toByte())"),
        2 => format!("{b}.putShort({o}, ({v}).toShort())"),
        _ => format!("{b}.putInt({o}, {v})"),
    }
}

/// `ByteArray` fields compare by content, so data classes holding bytes get
/// their own `equals`/`hashCode`.
fn array_equality(class: &str, fields: &[(String, bool)], pad: &str) -> String {
    if !fields.iter().any(|f| f.1) {
        return String::new();
    }
    let eq: Vec<String> = fields.iter().map(|(n, arr)| if *arr { format!("{n}.contentEquals(other.{n})") } else { format!("{n} == other.{n}") }).collect();
    let hash: Vec<String> = fields.iter().map(|(n, arr)| if *arr { format!("{n}.contentHashCode()") } else { format!("{n}.hashCode()") }).collect();
    let mut s = format!(" {{\n{pad}override fun equals(other: kotlin.Any?): kotlin.Boolean =\n{pad}    this === other || (other is {class} && {})\n", eq.join(" && "));
    write!(s, "{pad}override fun hashCode(): kotlin.Int {{\n{pad}    var h = 0\n").unwrap();
    for h in hash {
        writeln!(s, "{pad}    h = 31 * h + {h}").unwrap();
    }
    write!(s, "{pad}    return h\n{pad}}}\n{}}}", &pad[..pad.len().saturating_sub(4)]).unwrap();
    s
}
