use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use wna_abi::Wit;
use wna_gen::*;

const USAGE: &str = "usage: wna-gen <c|rust|swift|kotlin> --wit <dir> [--world <ns:pkg/world>]... [--iface <ns:pkg/iface>]... [--name <component>] [--package <kotlin prefix>] --out <dir>";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let backend = args.next().context(USAGE)?;
    let (mut wit, mut out, mut worlds, mut ifaces, mut name, mut package) = (None, None, vec![], vec![], None, None);
    while let Some(a) = args.next() {
        let mut val = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--wit" => wit = Some(PathBuf::from(val()?)),
            "--out" => out = Some(PathBuf::from(val()?)),
            "--world" => worlds.push(val()?),
            "--iface" => ifaces.push(val()?),
            "--name" => name = Some(val()?),
            "--package" => package = Some(val()?),
            _ => bail!("unknown argument {a}\n{USAGE}"),
        }
    }
    let wit = Wit::load(wit.context(USAGE)?)?;
    let out = out.context(USAGE)?;
    let mut selected = Vec::new();
    for i in &ifaces {
        selected.push(wit.interface(i)?);
    }
    for w in &worlds {
        let (imports, exports) = wit.world_interfaces(wit.world(w)?)?;
        selected.extend(imports.into_iter().chain(exports));
    }
    if worlds.is_empty() && ifaces.is_empty() {
        selected.extend(wit.resolve.interfaces.iter().filter(|(_, i)| i.name.is_some()).map(|(id, _)| id));
    }
    for &i in &selected {
        wit.check_v0(i)?;
    }
    let mut files = Files::new();
    match backend.as_str() {
        "c" => c::generate(&wit, &selected, &mut files),
        "rust" => {
            let [w] = worlds.as_slice() else { bail!("rust needs exactly one --world") };
            let opts = rust::Opts { name: name.clone().unwrap_or_else(|| "component".into()), additional_derives: vec![] };
            files.insert("bindings.rs".into(), rust::generate(&wit, wit.world(w)?, &opts)?);
        }
        "kotlin" => {
            let worlds = worlds.iter().map(|w| wit.world(w)).collect::<Result<Vec<_>>>()?;
            kotlin::generate(&wit, &worlds, &kotlin::Opts { package }, &mut files)?;
        }
        _ => bail!("unknown backend {backend}\n{USAGE}"),
    }
    write_files(&out, &files)
}
