//! Decode every Texture2D and render every Sprite of a game, timing each phase.

mod utils;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rabex::objects::ClassId;
use rabex::objects::pptr::FileId;
use rabex_env::handle::SerializedFileHandle;
use rabex_texture::Texture2D;
use rabex_texture::sprite::{self, Sprite, SpriteAtlas};

#[derive(Default)]
struct Timings {
    read: Duration,
    pixels: Duration,
    decode: Duration,
    render: Duration,
}

/// Error message -> (count, first example).
#[derive(Default)]
struct Errors(BTreeMap<String, (usize, String)>);
impl Errors {
    fn add(&mut self, error: anyhow::Error, example: impl FnOnce() -> String) {
        let entry = self
            .0
            .entry(format!("{error:#}"))
            .or_insert_with(|| (0, example()));
        entry.0 += 1;
    }
}

fn timed<T>(total: &mut Duration, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    *total += start.elapsed();
    result
}

fn main() -> Result<()> {
    let game = std::env::args().nth(1).unwrap_or_else(|| "silksong".into());
    let env = utils::find_game(&game)?.context("game not found")?;

    let start = Instant::now();
    let mut files = env.load_all_serialized_files()?;
    for name in ["globalgamemanagers.assets", "resources.assets"] {
        files.insert(name.to_owned(), env.load_serialized(name)?);
    }
    for i in 0.. {
        let name = format!("sharedassets{i}.assets");
        let Ok(file) = env.load_serialized(&name) else {
            break;
        };
        files.insert(name, file);
    }
    eprintln!("loaded {} files in {:?}", files.len(), start.elapsed());

    let mut errors = Errors::default();

    let mut t = Timings::default();
    let (mut textures, mut pixel_bytes, mut decoded_pixels) = (0usize, 0usize, 0u64);
    let start = Instant::now();
    for (path, file) in &files {
        for info in file
            .file
            .objects()
            .filter(|o| o.m_ClassID == ClassId::Texture2D)
        {
            let example = || format!("{path} @ {}", info.m_PathID);
            let result = (|| -> Result<()> {
                let texture: Texture2D =
                    timed(&mut t.read, || file.object_at(info.m_PathID)?.read())?;
                if texture.m_Width == 0 {
                    return Ok(());
                }
                let pixels = timed(&mut t.pixels, || {
                    texture.pixels(|s| env.read_stream(&s.path, s.offset, s.size))
                })?;
                let img = timed(&mut t.decode, || texture.decode(&pixels))?;
                pixel_bytes += pixels.len();
                decoded_pixels += img.width() as u64 * img.height() as u64;
                textures += 1;
                Ok(())
            })();
            if let Err(e) = result {
                errors.add(e, example);
            }
        }
    }
    let elapsed = start.elapsed();
    eprintln!(
        "textures: {textures} decoded in {elapsed:?} ({:.1} MB in, {:.1} Mpx out)",
        pixel_bytes as f64 / 1e6,
        decoded_pixels as f64 / 1e6
    );
    eprintln!(
        "  read {:?}, pixels {:?}, decode {:?}",
        t.read, t.pixels, t.decode
    );

    // Group sprites by the texture holding their pixels, so each texture is decoded once.
    let mut t = Timings::default();
    let start = Instant::now();
    type Sprites<'a> = Vec<(&'a String, &'a SerializedFileHandle<'a>, i64)>;
    let mut by_texture: BTreeMap<(usize, i64), Sprites> = BTreeMap::new();
    let mut atlases: BTreeMap<(usize, i64), SpriteAtlas> = BTreeMap::new();
    for (path, file) in &files {
        for info in file
            .file
            .objects()
            .filter(|o| o.m_ClassID == ClassId::Sprite)
        {
            let example = || format!("{path} @ {}", info.m_PathID);
            let result = (|| -> Result<(usize, i64)> {
                let sprite: Sprite = timed(&mut t.read, || file.object_at(info.m_PathID)?.read())?;
                if !sprite.is_atlas_packed() {
                    let texture = &sprite.m_RD.texture;
                    if texture.m_FileID != FileId::LOCAL {
                        bail!("sprite texture in another file");
                    }
                    return Ok((file_key(file), texture.m_PathID));
                }
                let atlas_ref = file.deref(sprite.m_SpriteAtlas)?;
                let key = (file_key(&atlas_ref.file), atlas_ref.path_id());
                if let std::collections::btree_map::Entry::Vacant(e) = atlases.entry(key) {
                    let atlas = timed(&mut t.read, || atlas_ref.read())?;
                    e.insert(atlas);
                }
                let rd = sprite.render_data(Some(&atlases[&key]))?;
                let texture = &rd.texture;
                if texture.m_FileID != FileId::LOCAL {
                    bail!("atlas texture in another file");
                }
                Ok((key.0, texture.m_PathID))
            })();
            match result {
                Ok(key) => by_texture
                    .entry(key)
                    .or_default()
                    .push((path, file, info.m_PathID)),
                Err(e) => errors.add(e, example),
            }
        }
    }
    let files_by_key: BTreeMap<usize, &SerializedFileHandle> =
        files.values().map(|f| (file_key(f), f)).collect();
    eprintln!(
        "sprites: resolved to {} textures in {:?}",
        by_texture.len(),
        start.elapsed()
    );

    let start = Instant::now();
    let (mut sprites, mut rendered_pixels) = (0usize, 0u64);
    for ((texture_file, texture_id), group) in &by_texture {
        let example = || format!("texture {texture_id}");
        let Some(texture_file) = files_by_key.get(texture_file) else {
            errors.add(anyhow::anyhow!("texture in an unlisted file"), example);
            continue;
        };
        let decoded = (|| -> Result<_> {
            let texture: Texture2D =
                timed(&mut t.read, || texture_file.object_at(*texture_id)?.read())?;
            let pixels = timed(&mut t.pixels, || {
                texture.pixels(|s| env.read_stream(&s.path, s.offset, s.size))
            })?;
            timed(&mut t.decode, || texture.decode(&pixels))
        })();
        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(e) => {
                errors.add(e, example);
                continue;
            }
        };
        for &(path, file, sprite_id) in group {
            let example = || format!("{path} @ {sprite_id}");
            let result = (|| -> Result<()> {
                let sprite: Sprite = timed(&mut t.read, || file.object_at(sprite_id)?.read())?;
                let atlas = match sprite.is_atlas_packed() {
                    true => {
                        let atlas_ref = file.deref(sprite.m_SpriteAtlas)?;
                        Some(&atlases[&(file_key(&atlas_ref.file), atlas_ref.path_id())])
                    }
                    false => None,
                };
                let rd = sprite.render_data(atlas)?;
                let img = timed(&mut t.render, || {
                    sprite::render_sprite(&sprite, rd, &decoded)
                })?;
                rendered_pixels += img.width() as u64 * img.height() as u64;
                sprites += 1;
                Ok(())
            })();
            if let Err(e) = result {
                errors.add(e, example);
            }
        }
    }
    eprintln!(
        "sprites: {sprites} rendered in {:?} ({:.1} Mpx out)",
        start.elapsed(),
        rendered_pixels as f64 / 1e6
    );
    eprintln!(
        "  read {:?}, pixels {:?}, decode {:?}, render {:?}",
        t.read, t.pixels, t.decode, t.render
    );

    if !errors.0.is_empty() {
        eprintln!("\nerrors:");
        for (error, (count, example)) in &errors.0 {
            eprintln!("{count:>8}  {error}  ({example})");
        }
        bail!("{} distinct errors", errors.0.len());
    }
    Ok(())
}

/// Identity of a loaded file, stable as the environment caches files.
fn file_key(file: &SerializedFileHandle) -> usize {
    std::ptr::from_ref(file.file) as usize
}
