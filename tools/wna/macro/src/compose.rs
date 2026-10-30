//! `wna::compose!("wna.compose.toml")`: the composition root, checked at
//! build time. Every import must be wired to an instance exporting a
//! compatible version; the graph must be acyclic; the shell world's imports
//! must all be provided.

use anyhow::{Context, Result, bail};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use wna_abi::Wit;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    /// Directory of WIT packages, relative to the compose file.
    wit: String,
    /// The world the platform shells implement.
    shell: String,
    instances: BTreeMap<String, Inst>,
    /// Shell import → providing instance.
    exports: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Inst {
    /// Component name; its descriptor symbol is derived from it.
    component: Option<String>,
    /// Rust crate implementing the component (else: a C symbol linked in).
    rust: Option<String>,
    world: Option<String>,
    feature_host: Option<HostCfg>,
    #[serde(default)]
    slots: Vec<SlotCfg>,
    #[serde(default)]
    imports: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct HostCfg {
    app_version: String,
    trusted_keys: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct SlotCfg {
    name: String,
    world: String,
    fallback: Option<String>,
    #[serde(default = "default_memory")]
    memory_mb: usize,
    fuel: Option<u64>,
}

fn default_memory() -> usize {
    64
}

const HOST_CONTROL: &str = "wna:host/features@0.1.0";

fn base(s: &str) -> &str {
    s.split('@').next().unwrap()
}

fn version(s: &str) -> &str {
    s.split_once('@').map(|(_, v)| v).unwrap_or("")
}

pub fn expand(path: &str) -> Result<TokenStream> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let file_path = manifest.join(path);
    let text = std::fs::read_to_string(&file_path).with_context(|| format!("reading {}", file_path.display()))?;
    let cfg: File = toml::from_str(&text).with_context(|| format!("parsing {}", file_path.display()))?;
    let wit_root = file_path.parent().unwrap().join(&cfg.wit);
    let wit = Wit::load(&wit_root)?;

    let world_ifaces = |spec: &str| -> Result<(Vec<String>, Vec<String>)> {
        let (i, e) = wit.world_interfaces(wit.world(spec)?)?;
        Ok((i.into_iter().map(|x| wit.iface_id(x)).collect(), e.into_iter().map(|x| wit.iface_id(x)).collect()))
    };

    // What each instance imports and exports (full ids with versions).
    let (shell_imports, shell_exports) = world_ifaces(&cfg.shell)?;
    let mut provides: HashMap<String, Vec<String>> = HashMap::from([("shell".to_string(), shell_exports.clone())]);
    let mut needs: HashMap<String, Vec<String>> = HashMap::new();
    for (name, inst) in &cfg.instances {
        if name == "shell" {
            bail!("`shell` is reserved for the platform shell");
        }
        let (imports, exports) = match (&inst.world, &inst.feature_host) {
            (Some(w), None) => world_ifaces(w)?,
            (None, Some(_)) => {
                let (mut imports, mut exports) = (Vec::new(), vec![HOST_CONTROL.to_string()]);
                for s in &inst.slots {
                    let (i, e) = world_ifaces(&s.world)?;
                    for x in i {
                        if !imports.contains(&x) {
                            imports.push(x);
                        }
                    }
                    exports.extend(e);
                }
                (imports, exports)
            }
            _ => bail!("instance `{name}`: give exactly one of `world` or `feature-host`"),
        };
        for i in &imports {
            if !inst.imports.contains_key(base(i)) {
                bail!("instance `{name}` imports {i} but `imports` does not wire it");
            }
        }
        for k in inst.imports.keys() {
            if !imports.iter().any(|i| base(i) == k) {
                bail!("instance `{name}` wires {k}, which its world does not import");
            }
        }
        provides.insert(name.clone(), exports);
        needs.insert(name.clone(), imports);
    }

    // Every wire must reach an instance exporting a compatible version.
    let check = |who: &str, iface: &str, from: &str| -> Result<()> {
        let list = provides.get(from).with_context(|| format!("{who}: {iface} is wired to unknown instance `{from}`"))?;
        let got = list.iter().find(|e| base(e) == base(iface)).with_context(|| format!("{who}: `{from}` does not export {}", base(iface)))?;
        if !wna_abi::version_compatible(version(iface), version(got)) {
            bail!("{who}: needs {iface} but `{from}` exports {got}");
        }
        Ok(())
    };
    for (name, inst) in &cfg.instances {
        for i in &needs[name] {
            check(name, i, &inst.imports[base(i)])?;
        }
        for s in &inst.slots {
            if let Some(fb) = &s.fallback {
                for e in world_ifaces(&s.world)?.1 {
                    check(&format!("{name} slot {}", s.name), &e, fb)?;
                }
            }
        }
    }
    for i in &shell_imports {
        let from = cfg.exports.get(base(i)).with_context(|| format!("the shell imports {i} but [exports] does not provide it"))?;
        check("shell", i, from)?;
    }

    // Instantiation order: dependencies first; a cycle is an error.
    let mut order: Vec<&String> = Vec::new();
    let mut state: HashMap<&str, u8> = HashMap::new();
    fn visit<'a>(n: &'a String, cfg: &'a File, state: &mut HashMap<&'a str, u8>, order: &mut Vec<&'a String>, path: &mut Vec<&'a str>) -> Result<()> {
        match state.get(n.as_str()) {
            Some(2) => return Ok(()),
            Some(1) => bail!("composition cycle: {} -> {n}; break it with a callback resource", path.join(" -> ")),
            _ => {}
        }
        state.insert(n, 1);
        path.push(n);
        let inst = &cfg.instances[n];
        let deps = inst.imports.values().chain(inst.slots.iter().filter_map(|s| s.fallback.as_ref()));
        for d in deps {
            if d != "shell" {
                let (k, _) = cfg.instances.get_key_value(d).unwrap();
                visit(k, cfg, state, order, path)?;
            }
        }
        path.pop();
        state.insert(n, 2);
        order.push(n);
        Ok(())
    }
    for n in cfg.instances.keys() {
        visit(n, &cfg, &mut state, &mut order, &mut Vec::new())?;
    }

    // Emit the plan.
    let mut externs = Vec::new();
    let instances = order.iter().map(|name| {
        let inst = &cfg.instances[*name];
        let imports = inst.imports.iter().map(|(k, v)| quote!((#k, #v)));
        let kind = if let Some(h) = &inst.feature_host {
            let (v, keys) = (&h.app_version, &h.trusted_keys);
            let slots = inst.slots.iter().map(|s| {
                let (n, w, m) = (&s.name, &s.world, s.memory_mb);
                let fb = match &s.fallback { Some(f) => quote!(Some(#f)), None => quote!(None) };
                let fuel = match s.fuel { Some(f) => quote!(Some(#f)), None => quote!(None) };
                quote!(::wna_host::compose::Slot { name: #n, world: #w, fallback: #fb, memory_mb: #m, fuel: #fuel })
            });
            quote!(::wna_host::compose::Kind::FeatureHost { app_version: #v, trusted_keys: &[#(#keys),*], slots: &[#(#slots),*] })
        } else {
            let comp = inst.component.clone().unwrap_or_else(|| (*name).clone());
            let sym = format_ident!("{}", wna_gen::rust::symbol(&comp));
            match &inst.rust {
                Some(krate) => {
                    let k = format_ident!("{}", krate.replace('-', "_"));
                    quote!(::wna_host::compose::Kind::Component(::#k::#sym))
                }
                None => {
                    externs.push(quote!(fn #sym() -> *const ::wna::abi::Component;));
                    quote!(::wna_host::compose::Kind::Component(#sym))
                }
            }
        };
        quote!(::wna_host::compose::Instance { name: #name, kind: #kind, imports: &[#(#imports),*] })
    }).collect::<Vec<_>>();

    let pkgs = wna_abi::package_dirs_in_order(&wit_root)?.into_iter().map(|d| {
        let mut files: Vec<_> = std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "wit")).collect();
        files.sort();
        let label = d.display().to_string();
        let files = files.iter().map(|f| {
            let p = f.display().to_string();
            quote!((#p, include_str!(#p)))
        });
        quote!((#label, &[#(#files),*]))
    });
    let shell_exports_lit = shell_exports.iter();
    let shell_imports_lit = shell_imports.iter().map(|i| {
        let from = &cfg.exports[base(i)];
        quote!((#i, #from))
    });
    let n_exports = shell_exports.len() as u32;
    let n_imports = shell_imports.len() as u32;
    let tracked = file_path.display().to_string();

    Ok(quote! {
        const _: &str = include_str!(#tracked);

        unsafe extern "C" { #(#externs)* }

        /// The validated composition plan.
        pub static PLAN: ::wna_host::compose::Plan = ::wna_host::compose::Plan {
            wit: &[#(#pkgs),*],
            shell_exports: &[#(#shell_exports_lit),*],
            shell_imports: &[#(#shell_imports_lit),*],
            instances: &[#(#instances),*],
        };

        static APP: ::std::sync::OnceLock<::wna_host::compose::App> = ::std::sync::OnceLock::new();

        /// Composes the app once; later calls return the same app.
        pub fn compose(shell: &[::wna::abi::Iface]) -> ::core::result::Result<&'static ::wna_host::compose::App, ::wna::Trap> {
            if let Some(app) = APP.get() {
                return Ok(app);
            }
            let app = ::wna_host::compose::compose(&PLAN, shell)?;
            Ok(APP.get_or_init(|| app))
        }

        /// Why `wna_compose` failed on this thread.
        ///
        /// # Safety
        /// `msg` must be writable.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn wna_compose_error(msg: *mut ::wna::abi::WnaList<u8>) {
            unsafe { ::wna::trap::last_trap(::core::ptr::null_mut(), msg) }
        }

        /// C entry point for shells: `shell` holds the shell world's exports
        /// in declaration order; `out` receives its imports.
        ///
        /// # Safety
        /// `shell` must hold `n_shell` entries and `out` room for `n_out`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn wna_compose(shell: *const ::wna::abi::Iface, n_shell: u32, out: *mut ::wna::abi::Iface, n_out: u32) -> i32 {
            ::wna::trap::guard(|| unsafe {
                if n_shell != #n_exports || n_out != #n_imports {
                    return Err(::wna::Trap::new(::wna::abi::TRAP_UNAVAILABLE, "shell built against a different shell world"));
                }
                let app = compose(::core::slice::from_raw_parts(shell, n_shell as usize))?;
                ::core::ptr::copy_nonoverlapping(app.imports.as_ptr(), out, app.imports.len());
                Ok(())
            })
        }
    })
}
