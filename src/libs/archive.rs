use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use mlua::{Table, Value};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::{LibCtx, PathScope};
use crate::error::LehuaError;

fn source_list(scope: &PathScope, sources: Value) -> mlua::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    match sources {
        Value::String(s) => out.push(scope.resolve(&s.to_str()?)?),
        Value::Table(t) => {
            for item in t.sequence_values::<String>() {
                out.push(scope.resolve(&item?)?);
            }
        }
        other => {
            return Err(LehuaError::msg(format!(
                "sources must be a path or an array of paths, got {}",
                other.type_name()
            ))
            .into())
        }
    }
    if out.is_empty() {
        return Err(LehuaError::msg("sources is empty").into());
    }
    let mut names: Vec<String> = Vec::with_capacity(out.len());
    for p in &out {
        if !p.exists() {
            return Err(LehuaError::msg(format!("source does not exist: {}", p.display())).into());
        }
        let name = entry_name(p)?;
        if names.contains(&name) {
            return Err(LehuaError::msg(format!(
                "two sources would both be stored as '{name}'"
            ))
            .into());
        }
        names.push(name);
    }
    Ok(out)
}

fn check_overlap(dest: &Path, sources: &[std::path::PathBuf], what: &str) -> mlua::Result<()> {
    for src in sources {
        if dest == src || (src.is_dir() && dest.starts_with(src)) {
            return Err(LehuaError::msg(format!(
                "{what}: the destination overlaps source '{}'",
                src.display()
            ))
            .into());
        }
    }
    Ok(())
}

fn entry_name(path: &Path) -> mlua::Result<String> {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| LehuaError::msg(format!("cannot archive '{}'", path.display())).into())
}

fn add_to_zip<W: Write + Seek>(
    zipw: &mut ZipWriter<W>,
    opts: SimpleFileOptions,
    name: &str,
    path: &Path,
) -> mlua::Result<()> {
    if path.is_dir() {
        zipw.add_directory(format!("{name}/"), opts)
            .map_err(mlua::Error::external)?;
        for entry in std::fs::read_dir(path).map_err(mlua::Error::external)? {
            let entry = entry.map_err(mlua::Error::external)?;
            let child_name = format!("{name}/{}", entry.file_name().to_string_lossy());
            add_to_zip(zipw, opts, &child_name, &entry.path())?;
        }
    } else {
        zipw.start_file(name, opts).map_err(mlua::Error::external)?;
        let mut f = File::open(path).map_err(mlua::Error::external)?;
        std::io::copy(&mut f, zipw).map_err(mlua::Error::external)?;
    }
    Ok(())
}

fn append_tar_sources<W: Write>(
    builder: &mut tar::Builder<W>,
    sources: &[std::path::PathBuf],
) -> mlua::Result<()> {
    for src in sources {
        let name = entry_name(src)?;
        if src.is_dir() {
            builder
                .append_dir_all(&name, src)
                .map_err(mlua::Error::external)?;
        } else {
            builder
                .append_path_with_name(src, &name)
                .map_err(mlua::Error::external)?;
        }
    }
    Ok(())
}

fn is_gzip(path: &Path) -> mlua::Result<bool> {
    let mut magic = [0u8; 2];
    let mut f = File::open(path).map_err(mlua::Error::external)?;
    let n = f.read(&mut magic).map_err(mlua::Error::external)?;
    Ok(n == 2 && magic == [0x1f, 0x8b])
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;
    let scope = PathScope::new(ctx);

    {
        let scope = scope.clone();
        t.set(
            "zip",
            lua.create_function(move |_, (dest, sources, opts): (String, Value, Option<Table>)| {
                let dest = scope.resolve(&dest)?;
                let sources = source_list(&scope, sources)?;
                check_overlap(&dest, &sources, "archive.zip")?;
                let mut file_opts = SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .large_file(true);
                if let Some(o) = &opts {
                    if let Some(level) = o.get::<Option<i64>>("level")? {
                        file_opts = file_opts.compression_level(Some(level));
                    }
                }
                let file = File::create(&dest).map_err(mlua::Error::external)?;
                let mut zipw = ZipWriter::new(file);
                for src in &sources {
                    let name = entry_name(src)?;
                    add_to_zip(&mut zipw, file_opts, &name, src)?;
                }
                zipw.finish().map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "unzip",
            lua.create_function(move |_, (src, dest): (String, String)| {
                let src = scope.resolve(&src)?;
                let dest = scope.resolve(&dest)?;
                let file = File::open(&src).map_err(mlua::Error::external)?;
                let mut archive = ZipArchive::new(file).map_err(mlua::Error::external)?;
                std::fs::create_dir_all(&dest).map_err(mlua::Error::external)?;
                archive.extract(&dest).map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "listZip",
            lua.create_function(move |lua, src: String| {
                let src = scope.resolve(&src)?;
                let file = File::open(&src).map_err(mlua::Error::external)?;
                let mut archive = ZipArchive::new(file).map_err(mlua::Error::external)?;
                let out = lua.create_table()?;
                for i in 0..archive.len() {
                    let entry = archive.by_index(i).map_err(mlua::Error::external)?;
                    let e = lua.create_table()?;
                    e.set("name", entry.name())?;
                    e.set("size", entry.size() as f64)?;
                    e.set("compressedSize", entry.compressed_size() as f64)?;
                    e.set("isDir", entry.is_dir())?;
                    out.raw_seti(i + 1, e)?;
                }
                Ok(out)
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "tar",
            lua.create_function(move |_, (dest, sources): (String, Value)| {
                let dest = scope.resolve(&dest)?;
                let sources = source_list(&scope, sources)?;
                check_overlap(&dest, &sources, "archive.tar")?;
                let gz = dest
                    .extension()
                    .map(|e| {
                        let e = e.to_string_lossy().to_ascii_lowercase();
                        e == "gz" || e == "tgz"
                    })
                    .unwrap_or(false);
                let file = File::create(&dest).map_err(mlua::Error::external)?;
                if gz {
                    let enc = GzEncoder::new(file, Compression::default());
                    let mut builder = tar::Builder::new(enc);
                    append_tar_sources(&mut builder, &sources)?;
                    let enc = builder.into_inner().map_err(mlua::Error::external)?;
                    enc.finish().map_err(mlua::Error::external)?;
                } else {
                    let mut builder = tar::Builder::new(file);
                    append_tar_sources(&mut builder, &sources)?;
                    builder.into_inner().map_err(mlua::Error::external)?;
                }
                Ok(())
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "untar",
            lua.create_function(move |_, (src, dest): (String, String)| {
                let src = scope.resolve(&src)?;
                let dest = scope.resolve(&dest)?;
                let file = File::open(&src).map_err(mlua::Error::external)?;
                let reader: Box<dyn Read> = if is_gzip(&src)? {
                    Box::new(GzDecoder::new(file))
                } else {
                    Box::new(file)
                };
                let mut archive = tar::Archive::new(reader);
                std::fs::create_dir_all(&dest).map_err(mlua::Error::external)?;
                archive.unpack(&dest).map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "listTar",
            lua.create_function(move |lua, src: String| {
                let src = scope.resolve(&src)?;
                let file = File::open(&src).map_err(mlua::Error::external)?;
                let reader: Box<dyn Read> = if is_gzip(&src)? {
                    Box::new(GzDecoder::new(file))
                } else {
                    Box::new(file)
                };
                let mut archive = tar::Archive::new(reader);
                let out = lua.create_table()?;
                for (i, entry) in (1usize..).zip(archive.entries().map_err(mlua::Error::external)?)
                {
                    let entry = entry.map_err(mlua::Error::external)?;
                    let e = lua.create_table()?;
                    e.set(
                        "name",
                        entry
                            .path()
                            .map_err(mlua::Error::external)?
                            .to_string_lossy()
                            .into_owned(),
                    )?;
                    e.set("size", entry.header().size().map_err(mlua::Error::external)? as f64)?;
                    e.set("isDir", entry.header().entry_type().is_dir())?;
                    out.raw_seti(i, e)?;
                }
                Ok(out)
            })?,
        )?;
    }

    Ok(Value::Table(t))
}
