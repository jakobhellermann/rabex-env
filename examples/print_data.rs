mod utils;

use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};
use byteorder::LE;
use rabex::objects::pptr::PathId;
use rabex::serde_typetree;
use rabex::typetree::TypeTreeNode;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let bundle_path = args
        .next()
        .context("expected bundle path as first argument")?;
    let path_id = args
        .next()
        .context("expected bundle path as second argument")?
        .parse::<PathId>()?;

    let env = utils::find_game("silksong")?.unwrap();
    let file = env.load_addressables_bundle_content(bundle_path)?;
    let object = file.object_at::<serde_json::Value>(path_id)?;
    let value = object.read()?;

    let tt = object.typetree()?;

    let serialized = serde_typetree::to_vec::<_, LE>(&value, tt)?;

    let a = ksdump("orig", tt, object.data())?;
    let b = ksdump("roundtrip", tt, &serialized)?;

    std::fs::write("orig", a)?;
    std::fs::write("roundtrip", b)?;

    // std::fs::write("c", format!("{:#}", serde_json::Value::deserialize(value)?))?;

    Ok(())
}

fn ksdump(prefix: &str, tt: &TypeTreeNode, data: &[u8]) -> Result<String> {
    let ksy = typetree2ksy::generate_yaml(tt)?;

    let out = std::env::temp_dir().join("rabex-env");
    std::fs::create_dir_all(&out)?;
    let out_data = out.join(format!("{prefix}-data"));
    let out_schema = out.join(format!("{prefix}-schema.ksy"));
    std::fs::write(&out_data, data)?;
    std::fs::write(&out_schema, ksy)?;

    println!("ksdump {} {}", out_data.display(), out_schema.display());
    let output = Command::new("ksdump")
        .arg(&out_data)
        .arg(&out_schema)
        .stdout(Stdio::piped())
        .spawn()?
        .wait_with_output()?;
    ensure!(output.status.success());

    let out = String::from_utf8(output.stdout)?;
    Ok(out)
}
