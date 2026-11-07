//! Mapping the objects of serialized files by class id.

use anyhow::{Context, Result};
use rabex::objects::ClassIdType;
use rabex::typetree::TypeTreeProvider;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::rewriter::SerializedFileRewriter;

/// Replace every object of class `T` by what `map` makes of it. Objects of other classes
/// keep their bytes.
pub fn map_class<T, P>(
    rewriter: &mut SerializedFileRewriter<'_, P>,
    map: &mut impl FnMut(T) -> Result<T>,
) -> Result<()>
where
    T: DeserializeOwned + Serialize + ClassIdType,
    P: TypeTreeProvider,
{
    let infos: Vec<_> = rewriter.objects().cloned().collect();
    for info in infos {
        if info.m_ClassID != T::CLASS_ID {
            continue;
        }
        let context = || format!("{:?} {}", T::CLASS_ID, info.m_PathID);
        let object: T = rewriter.read(&info).with_context(context)?;
        let object = map(object).with_context(context)?;
        rewriter.replace(&info, &object).with_context(context)?;
    }
    Ok(())
}
