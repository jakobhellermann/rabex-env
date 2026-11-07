//! The rules for engine fields whose value in the target is not the source field of
//! their name, as tables the re-encode plan consults per field.
//!
//! A field the target names carries over from the source field of that name, and a
//! source field the target does not name is dropped. The tables are where that is wrong:
//! fields Unity renamed (one entry for both directions), fields Unity 6 dropped that the
//! older form still stores, and fields of single classes that took another shape or have
//! to agree with what changed around them. Which applies follows from the two trees, so
//! no version decides anything here.

use anyhow::{Context, Result, bail};
use rabex::UnityVersion;
use rabex::typetree::TypeTreeNode;

use crate::value::{Value, field, has_field};

/// How the two spellings of one field convert into each other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// The field only changed names.
    Same,
    /// A bool that became an enum: Unity numbers the bool's `true` meaning as the given
    /// variant and `false` as 0. Further variants added with the enum fail rather than
    /// pass as `false`.
    // TODO: whether a further variant has a fair `false` (or `true`) reading is open per
    // field — Collider2D's Intersect and Difference, ParticleSystem's Custom velocity.
    BoolAsEnum { set: i64 },
    /// A bool that became an enum with one variant for each of its values and further
    /// ones the bool has no room for: those fail rather than pick a side.
    BoolAsTwoOf { on: i64, off: i64 },
}

/// One field under both of its names, newer spelling first.
struct Change {
    new: &'static str,
    old: &'static str,
    shape: Shape,
}

const CHANGES: &[Change] = &[
    // Rigidbody2D's drag became damping in Unity 6, matching Rigidbody.
    Change {
        new: "m_LinearDamping",
        old: "m_LinearDrag",
        shape: Shape::Same,
    },
    Change {
        new: "m_AngularDamping",
        old: "m_AngularDrag",
        shape: Shape::Same,
    },
    // Animator's flag lost the "Controller" in its name.
    Change {
        new: "m_KeepAnimatorStateOnDisable",
        old: "m_KeepAnimatorControllerStateOnDisable",
        shape: Shape::Same,
    },
    // Collider2D's composite flag became an operation: None, Merge, Intersect,
    // Difference. The flag said Merge.
    Change {
        new: "m_CompositeOperation",
        old: "m_UsedByComposite",
        shape: Shape::BoolAsEnum { set: 1 },
    },
    // ParticleSystem's inherit-velocity flag became a mode: Transform, Rigidbody,
    // Custom. The flag said Rigidbody.
    Change {
        new: "emitterVelocityMode",
        old: "useRigidbodyForVelocity",
        shape: Shape::BoolAsEnum { set: 1 },
    },
    // The 3D physics' automatic stepping became a mode: FixedUpdate, Update, Script.
    // Automatic was the fixed step: Hollow Knight's true in 2020.2 is 0 in its own
    // 6000.0 build. Stepping in Update has no automatic-flag spelling.
    Change {
        new: "m_SimulationMode",
        old: "m_AutoSimulation",
        shape: Shape::BoolAsTwoOf { on: 0, off: 2 },
    },
    // Physics2D's restitution cut-off was renamed after what it decides: Hollow Knight
    // writes 1.0 as the velocity threshold in 2020.2 and as the bounce threshold in its
    // own 6000.0 build.
    Change {
        new: "m_BounceThreshold",
        old: "m_VelocityThreshold",
        shape: Shape::Same,
    },
    // A quality level's texture resolution cut, which the scripting API renamed from
    // masterTextureLimit to globalTextureMipmapLimit along with the stored field: 0 is
    // full resolution, every step halves it.
    // TODO: Hollow Knight writes 0 on both sides, nothing else was observed; that the
    // steps count alike rests on the API's documentation of the rename.
    Change {
        new: "globalTextureMipmapLimit",
        old: "textureQuality",
        shape: Shape::Same,
    },
    // The NavMesh build's height mesh, which is what accurate placement was done with,
    // took the mechanism's name; it sits at the same place in the settings.
    // TODO: Hollow Knight writes 0 on both sides; a build that turned it on would show
    // the flag carries over as the same value.
    Change {
        new: "buildHeightMesh",
        old: "accuratePlacement",
        shape: Shape::Same,
    },
    // The Android resizable window took the name of Android's own setting; Unity 6's
    // editor: "resizableWindow has been deprecated and renamed to match Android
    // documentation. Please use resizeableActivity instead. (UnityUpgradable) ->
    // resizeableActivity".
    Change {
        new: "androidResizeableActivity",
        old: "androidResizableWindow",
        shape: Shape::Same,
    },
    // PlayerSettings' HDR output depth lost its Direct3D prefix once more backends
    // supported HDR output.
    // TODO: only 0 (10 bit) is observed on either side (Hollow Knight 2020.2 and 6000,
    // A Short Hike, Silksong all write 0), so the variants being numbered alike is
    // assumed. A build of either version with 16 bit HDR output would settle it.
    Change {
        new: "hdrBitDepth",
        old: "D3DHDRBitDepth",
        shape: Shape::Same,
    },
];

/// What a rule reads: the source object around the field it decides. Only what is asked
/// for is decoded.
pub trait Source {
    /// A field of the source struct the decided field lands in, `None` if it lacks one.
    fn field(&self, name: &str) -> Result<Option<Value>>;
    /// The whole source object.
    fn object(&self) -> Result<&Value>;
    /// The element the struct is of the innermost array around it.
    fn index(&self) -> Result<usize>;
    /// The version the object is written for.
    fn target(&self) -> &UnityVersion;
    /// Note what the value written cannot say exactly; the retarget reports it, counted
    /// per message, and goes on.
    fn warn(&self, message: String);
}

/// A field one form stores and the other lacks, with the value Unity's own builds of the
/// form that stores it write: the older form's fields Unity 6 dropped, filled in going
/// down, and the fields it added, going up.
struct Absent {
    /// The struct types carrying the field, so the name is read in their context.
    owners: &'static [&'static str],
    field: &'static str,
    /// The value: some point into files whose place in the external list only the
    /// object's other fields tell.
    value: fn(&dyn Source) -> Result<Value>,
    /// Where the value comes from.
    evidence: &'static str,
}

const ABSENT: &[Absent] = &[
    // Unity 6 removed auto-generated lighting, leaving on-demand baking as the only
    // workflow; the enum that chose between them went with it.
    Absent {
        owners: &["LightingSettings"],
        field: "m_GIWorkflowMode",
        value: |_| Ok(Value::I64(1)),
        evidence: "Hollow Knight's own 2020.2 build writes 1 (OnDemand)",
    },
    // PlayerSettings of platforms and options Unity 6 dropped. Silksong's 6000.0 data
    // carries none of them; the values are what Hollow Knight's own 2020.2.2f1 build
    // writes, and A Short Hike's 2021.3 build agrees on all but the aspect ratios it
    // has no opinion on here.
    Absent {
        owners: &["PlayerSettings"],
        field: "iosAllowHTTPDownload",
        // not Unity 6's insecureHttpOption under an old name: the same game, Hollow
        // Knight, writes true here in 2020.2 and 0 (not allowed) there in 6000.0
        value: |_| Ok(Value::Bool(true)),
        evidence: "Hollow Knight 2020.2.2f1 and A Short Hike 2021.3 write true",
    },
    Absent {
        owners: &["PlayerSettings"],
        field: "captureSingleScreen",
        value: |_| Ok(Value::Bool(false)),
        evidence: "Hollow Knight 2020.2.2f1 and A Short Hike 2021.3 write false",
    },
    Absent {
        owners: &["PlayerSettings"],
        field: "stadiaPresentMode",
        value: |_| Ok(Value::I64(0)),
        evidence: "Hollow Knight 2020.2.2f1 writes 0",
    },
    Absent {
        owners: &["PlayerSettings"],
        field: "stadiaTargetFramerate",
        value: |_| Ok(Value::I64(0)),
        evidence: "Hollow Knight 2020.2.2f1 writes 0",
    },
    // The ratios the startup resolution dialog offered; Unity 6 has no such dialog.
    Absent {
        owners: &["PlayerSettings"],
        field: "m_SupportedAspectRatios",
        value: |_| {
            Ok(Value::Map(
                [
                    ("4:3", false),
                    ("5:4", false),
                    ("16:10", false),
                    ("16:9", true),
                    ("Others", false),
                ]
                .into_iter()
                .map(|(ratio, on)| (Value::String(ratio.into()), Value::Bool(on)))
                .collect(),
            ))
        },
        evidence: "Hollow Knight 2020.2.2f1 writes only 16:9 on",
    },
    // The built-in shader of the legacy deferred (pre-pass) lighting path, which Unity
    // 6 removed together with the shader. It lives in `unity_builtin_extra`, the file
    // the deferred shader the object still names lives in, at the path 2020.2 gives it.
    Absent {
        owners: &["GraphicsSettings"],
        field: "m_LegacyDeferred",
        value: |source| {
            let deferred = source.field("m_Deferred")?;
            let builtin_extra = deferred
                .as_ref()
                .and_then(|deferred| field(deferred, "m_Shader"))
                .and_then(|shader| field(shader, "m_FileID"))
                .context("no m_Deferred shader to find unity_builtin_extra by")?
                .clone();
            Ok(Value::Map(
                [
                    ("m_Mode", Value::I64(1)),
                    (
                        "m_Shader",
                        Value::Map(
                            [("m_FileID", builtin_extra), ("m_PathID", Value::I64(63))]
                                .into_iter()
                                .map(|(name, value)| (Value::String(name.into()), value))
                                .collect(),
                        ),
                    ),
                ]
                .into_iter()
                .map(|(name, value)| (Value::String(name.into()), value))
                .collect(),
            ))
        },
        evidence: "Hollow Knight 2020.2.2f1: built-in mode, Hidden/Internal-PrePassLighting \
                   at path 63 of its unity_builtin_extra",
    },
    // Unity 6 keeps only the persistent contact manifold; 2020.2 still let the legacy
    // generation be chosen.
    Absent {
        owners: &["PhysicsManager"],
        field: "m_ContactsGeneration",
        value: |_| Ok(Value::I64(1)),
        evidence: "Hollow Knight 2020.2.2f1 writes 1 (persistent contact manifold)",
    },
    Absent {
        owners: &["PhysicsManager"],
        field: "m_EnableUnifiedHeightmaps",
        value: |_| Ok(Value::Bool(true)),
        evidence: "Hollow Knight 2020.2.2f1 writes true",
    },
    // The broadphase region of the sweep-and-prune variant Unity 6 dropped.
    Absent {
        owners: &["PhysicsManager"],
        field: "m_WorldBounds",
        value: |_| {
            let vector = |v: f32| {
                Value::Map(
                    ["x", "y", "z"]
                        .into_iter()
                        .map(|axis| (Value::String(axis.into()), Value::F32(v)))
                        .collect(),
                )
            };
            Ok(Value::Map(
                [("m_Center", vector(0.0)), ("m_Extent", vector(250.0))]
                    .into_iter()
                    .map(|(name, value)| (Value::String(name.into()), value))
                    .collect(),
            ))
        },
        evidence: "Hollow Knight 2020.2.2f1 writes center 0, extent 250",
    },
    Absent {
        owners: &["PhysicsManager"],
        field: "m_WorldSubdivisions",
        value: |_| Ok(Value::I64(8)),
        evidence: "Hollow Knight 2020.2.2f1 writes 8",
    },
    // The identity of a build. Unity 6 no longer stores one here; the build's
    // authentication token is unique per build as well, so the GUID is made from it:
    // stable for the same build, distinct across builds.
    Absent {
        owners: &["BuildSettings"],
        field: "buildGUID",
        value: |source| {
            let Some(Value::String(token)) = source.field("m_AuthToken")? else {
                bail!("no m_AuthToken to make the build's GUID from");
            };
            Ok(guid_of(&token))
        },
        evidence: "Hollow Knight 2020.2.2f1 writes four words; their value is per build",
    },
    Absent {
        owners: &["BuildSettings"],
        field: "isDebugBuild",
        value: |_| Ok(Value::Bool(false)),
        evidence: "Hollow Knight 2020.2.2f1, a release build, writes false",
    },
    // The rendering layers a renderer starts in; Unity 6 moved the setting out of
    // GraphicsSettings into the render pipeline.
    Absent {
        owners: &["GraphicsSettings"],
        field: "m_DefaultRenderingLayerMask",
        value: |_| Ok(Value::U32(1)),
        evidence: "Hollow Knight 2020.2.2f1 writes 1",
    },
    // Unity 6 dropped the send slot from a mixer group's constant. The slot a group
    // gets is past every slot the mixer already has, in group order, matching the
    // slots the snapshots grow by (`AudioMixerConstant.snapshots` below).
    Absent {
        owners: &["GroupConstant"],
        field: "sendIndex",
        value: |source| {
            let first_new_slot = snapshot_size(source.object()?)?;
            Ok(Value::U32(u32::try_from(first_new_slot + source.index()?)?))
        },
        evidence: "Unity's own 2020.2 mixers give every group a slot in the snapshots' \
                   value space",
    },
    // Fields Unity 6 added, with what its builds write. The evidence is Silksong (6000.0.50f1)
    // and Hollow Knight (6000.0.61f1, a project upgraded from 2020.2): every object of the
    // classes measured holds the value, the counts are across both games. The renderer and
    // collider fields belong to the base classes Renderer and Collider2D, which every
    // subclass's tree starts with alike; subclasses neither game has objects of
    // (LineRenderer, SkinnedMeshRenderer, TilemapRenderer, TilemapCollider2D) take what
    // the measured ones hold.
    Absent {
        owners: &[
            "BoxCollider2D",
            "CircleCollider2D",
            "PolygonCollider2D",
            "EdgeCollider2D",
            "CapsuleCollider2D",
            "CompositeCollider2D",
            "TilemapCollider2D",
        ],
        field: "m_CompositeOrder",
        value: |_| Ok(Value::I32(0)),
        evidence: "0 in all 191,158 colliders of the classes",
    },
    Absent {
        owners: &[
            "MeshRenderer",
            "SpriteRenderer",
            "ParticleSystemRenderer",
            "TrailRenderer",
            "SpriteMask",
            "LineRenderer",
            "SkinnedMeshRenderer",
            "TilemapRenderer",
        ],
        field: "m_RayTracingAccelStructBuildFlagsOverride",
        value: |_| Ok(Value::U8(0)),
        evidence: "0 in all 1,391,091 renderers of the classes",
    },
    Absent {
        owners: &[
            "MeshRenderer",
            "SpriteRenderer",
            "ParticleSystemRenderer",
            "TrailRenderer",
            "SpriteMask",
            "LineRenderer",
            "SkinnedMeshRenderer",
            "TilemapRenderer",
        ],
        field: "m_RayTracingAccelStructBuildFlags",
        value: |_| Ok(Value::U8(1)),
        evidence: "1 in all 1,391,091 renderers of the classes",
    },
    Absent {
        owners: &["LightmapSettings"],
        field: "m_BakeOnSceneLoad",
        value: |_| Ok(Value::I32(0)),
        evidence: "0 in all 1,092 scenes' lightmap settings",
    },
    Absent {
        owners: &["PlayerSettings"],
        field: "m_SpriteBatchMaxVertexCount",
        value: |_| Ok(Value::I32(65535)),
        evidence: "65535 in both games' player settings",
    },
    Absent {
        owners: &["PlayerSettings"],
        field: "androidApplicationEntry",
        value: |_| Ok(Value::I32(1)),
        evidence: "1 in both games' player settings",
    },
    Absent {
        owners: &["PlayerSettings"],
        field: "meshDeformation",
        value: |_| Ok(Value::I32(0)),
        evidence: "0 in both games' player settings",
    },
    Absent {
        owners: &["PlayerSettings"],
        field: "androidMinAspectRatio",
        value: |_| Ok(Value::F32(1.0)),
        evidence: "1.0 in both games' player settings",
    },
    Absent {
        owners: &["PlayerSettings"],
        field: "androidVulkanDenyFilterList",
        value: |_| Ok(Value::Seq(Vec::new())),
        evidence: "empty in both games' player settings",
    },
    Absent {
        owners: &["PlayerSettings"],
        field: "androidVulkanAllowFilterList",
        value: |_| Ok(Value::Seq(Vec::new())),
        evidence: "empty in both games' player settings",
    },
    Absent {
        owners: &[
            "MeshRenderer",
            "SpriteRenderer",
            "ParticleSystemRenderer",
            "TrailRenderer",
            "SpriteMask",
            "LineRenderer",
            "SkinnedMeshRenderer",
            "TilemapRenderer",
        ],
        field: "m_SmallMeshCulling",
        value: |_| Ok(Value::U8(1)),
        evidence: "1 in all but one of the 1,391,091 renderers of the classes, all of Hollow \
                   Knight's",
    },
    Absent {
        owners: &["SpriteMask"],
        field: "m_MaskSource",
        value: |_| Ok(Value::I32(0)),
        evidence: "0 in all 161 of Silksong's sprite masks",
    },
    Absent {
        owners: &["HingeJoint2D"],
        field: "m_UseConnectedAnchor",
        value: |_| Ok(Value::Bool(true)),
        evidence: "true in all 13,591 of Silksong's 2D hinge joints",
    },
    Absent {
        owners: &["Light"],
        field: "m_ForceVisible",
        value: |_| Ok(Value::Bool(false)),
        evidence: "false in all 104 of Hollow Knight's lights",
    },
    Absent {
        owners: &["Light"],
        field: "m_LightUnit",
        value: |_| Ok(Value::I32(1)),
        evidence: "1 in all 104 of Hollow Knight's lights",
    },
    Absent {
        owners: &["Light"],
        field: "m_LuxAtDistance",
        value: |_| Ok(Value::F32(1.0)),
        evidence: "1.0 in all 104 of Hollow Knight's lights",
    },
    Absent {
        owners: &["Light"],
        field: "m_EnableSpotReflector",
        value: |_| Ok(Value::Bool(true)),
        evidence: "true in all 104 of Hollow Knight's lights",
    },
    // project settings, one object per game, alike in both
    Absent {
        owners: &["Physics2DSettings"],
        field: "m_ContactThreshold",
        value: |_| Ok(Value::F32(0.0)),
        evidence: "0.0 in both games",
    },
    Absent {
        owners: &["Physics2DSettings"],
        field: "m_SimulationLayers",
        value: |_| Ok(structure([("m_Bits", Value::U32(u32::MAX))])),
        evidence: "every layer in both games",
    },
    Absent {
        owners: &["Physics2DSettings"],
        field: "m_MaxSubStepCount",
        value: |_| Ok(Value::I32(4)),
        evidence: "4 in both games",
    },
    Absent {
        owners: &["Physics2DSettings"],
        field: "m_MinSubStepFPS",
        value: |_| Ok(Value::F32(30.0)),
        evidence: "30.0 in both games",
    },
    Absent {
        owners: &["Physics2DSettings"],
        field: "m_UseSubStepping",
        value: |_| Ok(Value::Bool(false)),
        evidence: "false in both games",
    },
    Absent {
        owners: &["Physics2DSettings"],
        field: "m_UseSubStepContacts",
        value: |_| Ok(Value::Bool(false)),
        evidence: "false in both games",
    },
    Absent {
        owners: &["GraphicsSettings"],
        field: "m_CurrentRenderPipelineGlobalSettings",
        value: |_| {
            Ok(structure([
                ("m_FileID", Value::I32(0)),
                ("m_PathID", Value::I64(0)),
            ]))
        },
        evidence: "a null reference in both games",
    },
    Absent {
        owners: &["PhysicsManager"],
        field: "m_GenerateOnTriggerStayEvents",
        value: |_| Ok(Value::Bool(true)),
        evidence: "true in both games",
    },
    Absent {
        owners: &["PhysicsManager"],
        field: "m_ScratchBufferChunkCount",
        value: |_| Ok(Value::U32(4)),
        evidence: "4 in both games",
    },
    Absent {
        owners: &["PhysicsManager"],
        field: "m_CurrentBackendId",
        value: |_| Ok(Value::U32(4_072_204_805)),
        evidence: "4072204805 in both games, the one backend 6000.0 ships",
    },
    Absent {
        owners: &["TagManager"],
        field: "m_RenderingLayers",
        value: |_| Ok(Value::Seq(vec![Value::String("Default".to_owned())])),
        evidence: "the one layer \"Default\" in both games",
    },
    Absent {
        owners: &["VFXManager"],
        field: "m_MaxCapacity",
        value: |_| Ok(Value::U32(100_000_000)),
        evidence: "100000000 in both games",
    },
    // Unity 6 split the animator's AnimatePhysics update mode (1) in two: the mode Fixed,
    // still 1, the timing, and m_AnimatePhysics, the transforms synced for physics. Its
    // own deprecation says so ("Use AnimatorUpdateMode.Fixed to evaluate in FixedUpdate
    // time and Animator.animatePhysics to sync transforms for physics"), and 2022.3's
    // animatePhysics property already read as `updateMode == AnimatePhysics`.
    Absent {
        owners: &["Animator"],
        field: "m_AnimatePhysics",
        value: |source| match source.field("m_UpdateMode")? {
            Some(Value::I32(mode)) => Ok(Value::Bool(mode == 1)),
            other => bail!("an animator update mode of {other:?}"),
        },
        evidence: "Silksong and Hollow Knight: true in the one animator of mode 1, false in \
                   all 50,293 of modes 0 and 2",
    },
    // Explicit dynamic scaling, applied by a call to ApplyDynamicScale, is a mode 2022.3
    // has no property for; a render texture of it scaled the implicit way or not at all.
    Absent {
        owners: &["RenderTexture"],
        field: "m_UseDynamicScaleExplicit",
        value: |_| Ok(Value::Bool(false)),
        evidence: "2022.3.62f3's RenderTexture has no useDynamicScaleExplicit",
    },
    // A quality level's adaptive vsync; Silksong turns it on, Hollow Knight's upgrade wrote
    // it off.
    Absent {
        owners: &["QualitySetting"],
        field: "adaptiveVsync",
        value: |_| Ok(Value::Bool(false)),
        evidence: "false in Hollow Knight's upgraded quality level",
    },
    Absent {
        owners: &["QualitySetting"],
        field: "adaptiveVsyncExtraA",
        value: |_| Ok(Value::I32(0)),
        evidence: "0 in Hollow Knight's upgraded quality level",
    },
    Absent {
        owners: &["QualitySetting"],
        field: "adaptiveVsyncExtraB",
        value: |_| Ok(Value::I32(0)),
        evidence: "0 in Hollow Knight's upgraded quality level",
    },
    // Going down, a light's shape out of the type Unity 6 folded it into, see the rules of
    // Light.m_Type.
    Absent {
        owners: &["Light"],
        field: "m_Shape",
        value: |source| match source.field("m_Type")? {
            Some(Value::I32(5)) => Ok(Value::I32(1)),
            Some(Value::I32(6)) => Ok(Value::I32(2)),
            Some(Value::I32(_)) => Ok(Value::I32(0)),
            other => bail!("a light type of {other:?}"),
        },
        evidence: "LightShape and LightType of the 2022.3.62f3 and 6000.0.61f1 assemblies",
    },
    // The GUID of the shader's asset in the project, a random one the editor gives each
    // asset; the 6000.0 player looks shaders up by it for GraphicsStateCollections only
    // ("ShaderAssetGUID %s referenced in GraphicsStateCollection %s was not found" in its
    // UnityPlayer.so), which no older project has. Older builds do not store it, so it
    // is made from what names the shader: its name, unique among a game's shaders.
    // TODO: a GraphicsStateCollection recorded in the editor names the project's real
    // GUIDs, which these are not; they only keep the shaders apart.
    Absent {
        owners: &["Shader"],
        field: "m_AssetGUID",
        value: |source| {
            let form = source
                .field("m_ParsedForm")?
                .context("a shader without parsed form")?;
            let Some(Value::String(name)) = field(&form, "m_Name") else {
                bail!("a shader without a name");
            };
            Ok(guid_of(name))
        },
        evidence: "none: no build that stores it has the shaders' editor GUIDs to compare",
    },
    // The byte a build writes into PlayerSettings, see BUILD_ONLY_FIELDS.
    Absent {
        owners: &["PlayerSettings"],
        field: "captureStartupLogs",
        value: |_| Ok(Value::Bool(false)),
        evidence: "false in both games' player settings, read with the build's tree",
    },
    // What an audio source plays became a resource, a clip or one of the containers Unity 6
    // added. A source that plays a clip names it in both.
    Absent {
        owners: &["AudioSource"],
        field: "m_Resource",
        value: |source| {
            source
                .field("m_audioClip")?
                .context("an audio source without a clip")
        },
        evidence: "the clip in 35,674 of the games' 41,925 audio sources; the rest have no \
                   clip and name a container",
    },
    Absent {
        owners: &["Sprite"],
        field: "m_ScriptableObjects",
        value: |_| Ok(Value::Seq(Vec::new())),
        evidence: "empty in all 22,394 of Silksong's sprites",
    },
    // 2D physics materials gained combine modes; Hollow Knight's own upgrade from 2020.2,
    // which had none, wrote these
    Absent {
        owners: &["PhysicsMaterial2D"],
        field: "m_FrictionCombine",
        value: |_| Ok(Value::I32(1)),
        evidence: "1 in all 15 of the games' 2D physics materials",
    },
    Absent {
        owners: &["PhysicsMaterial2D"],
        field: "m_BounceCombine",
        value: |_| Ok(Value::I32(4)),
        evidence: "4 in all 15 of the games' 2D physics materials",
    },
];

/// A field of one class whose value is not the source field of its name: one that took
/// another shape, or that has to agree with what changed around it.
struct FieldRule {
    /// The struct type carrying the field, in the target's tree.
    owner: &'static str,
    field: &'static str,
    /// Whether the rule decides the field between these forms of the owner.
    when: fn(source: &TypeTreeNode, target: &TypeTreeNode) -> bool,
    value: fn(&dyn Source) -> Result<Value>,
    /// Source fields the target lacks that the value carries, so they are not dropped.
    folds: &'static [&'static str],
}

const FIELD_RULES: &[FieldRule] = &[
    // Unity 6 stores the fixed step as a rational time: a count of ticks at a rate of
    // numerator/denominator ticks per second (2822399 at 141120000/1 for 0.02 s in
    // Silksong). The older form stores the seconds.
    FieldRule {
        owner: "TimeManager",
        field: "Fixed Timestep",
        when: |source, target| {
            let kind =
                |tree: &TypeTreeNode| child(tree, "Fixed Timestep").map(|c| c.children.is_empty());
            kind(source) == Some(false) && kind(target) == Some(true)
        },
        value: |source| {
            let time = source
                .field("Fixed Timestep")?
                .context("no Fixed Timestep")?;
            let time = &time;
            let number = |value: Option<&Value>, what: &str| -> Result<f64> {
                match value {
                    Some(Value::I64(n)) => Ok(*n as f64),
                    Some(Value::U32(n)) => Ok(*n as f64),
                    Some(Value::I32(n)) => Ok(*n as f64),
                    Some(Value::U64(n)) => Ok(*n as f64),
                    other => bail!("{what} is {other:?}"),
                }
            };
            let count = number(field(time, "m_Count"), "m_Count")?;
            let rate = field(time, "m_Rate").context("no m_Rate")?;
            let numerator = number(field(rate, "m_Numerator"), "m_Numerator")?;
            let denominator = number(field(rate, "m_Denominator"), "m_Denominator")?;
            anyhow::ensure!(numerator != 0.0, "a rate of zero ticks per second");
            Ok(Value::F32((count * denominator / numerator) as f32))
        },
        folds: &[],
    },
    // Downwards, the update mode stays: Fixed is the old AnimatePhysics, which also synced
    // the transforms. An animator that does one without the other has no old spelling.
    FieldRule {
        owner: "Animator",
        field: "m_UpdateMode",
        when: |source, target| {
            has_field(source, "m_AnimatePhysics") && !has_field(target, "m_AnimatePhysics")
        },
        value: |source| {
            let Some(Value::I32(mode)) = source.field("m_UpdateMode")? else {
                bail!("an animator without an update mode");
            };
            let Some(Value::Bool(sync)) = source.field("m_AnimatePhysics")? else {
                bail!("an animator without m_AnimatePhysics");
            };
            if sync != (mode == 1) {
                source.warn(format!(
                    "Animator: update mode {mode} with animatePhysics {sync}, written as mode \
                     {mode}, which {}",
                    if mode == 1 {
                        "also syncs the transforms"
                    } else {
                        "does not sync them"
                    }
                ));
            }
            Ok(Value::I32(mode))
        },
        folds: &["m_AnimatePhysics"],
    },
    // Unity 6 folded a spot light's shape into its type: LightShape Cone, Pyramid, Box
    // (m_Shape 0, 1, 2) of a Spot (m_Type 0) are the types Spot, Pyramid, Box (0, 5, 6) —
    // "Use LightType.Spot, LightType.Pyramid, or LightType.Box instead", its deprecation of
    // LightShape says. The other types had no shape.
    FieldRule {
        owner: "Light",
        field: "m_Type",
        when: |source, target| has_field(source, "m_Shape") && !has_field(target, "m_Shape"),
        value: |source| {
            let Some(Value::I32(kind)) = source.field("m_Type")? else {
                bail!("a light without a type");
            };
            let Some(Value::I32(shape)) = source.field("m_Shape")? else {
                bail!("a light without a shape");
            };
            Ok(Value::I32(match (kind, shape) {
                (0, 0) => 0,
                (0, 1) => 5,
                (0, 2) => 6,
                (0, other) => bail!("a spot light of shape {other}"),
                (kind, _) => kind,
            }))
        },
        folds: &["m_Shape"],
    },
    FieldRule {
        owner: "Light",
        field: "m_Type",
        when: |source, target| !has_field(source, "m_Shape") && has_field(target, "m_Shape"),
        value: |source| {
            let Some(Value::I32(kind)) = source.field("m_Type")? else {
                bail!("a light without a type");
            };
            Ok(Value::I32(match kind {
                5 | 6 => 0,
                7 => {
                    source.warn(
                        "Light: a tube light, which the older form has no type for, \
                                 written as type 7"
                            .to_owned(),
                    );
                    7
                }
                kind => kind,
            }))
        },
        folds: &[],
    },
    // Upwards, the seconds become ticks at Unity's rate, 141120000 per second, the count
    // truncated: Hollow Knight's 0.02 (as a float 0.0199999995…) in 2020.2 is 2822399 in
    // its upgraded 6000.0 build, not the 2822400 rounding would give.
    FieldRule {
        owner: "TimeManager",
        field: "Fixed Timestep",
        when: |source, target| {
            let kind =
                |tree: &TypeTreeNode| child(tree, "Fixed Timestep").map(|c| c.children.is_empty());
            kind(source) == Some(true) && kind(target) == Some(false)
        },
        value: |source| {
            const RATE: u32 = 141_120_000;
            let Some(Value::F32(seconds)) = source.field("Fixed Timestep")? else {
                bail!("a Fixed Timestep that is not a float");
            };
            let count = (f64::from(seconds) * f64::from(RATE)).floor();
            anyhow::ensure!(
                count >= 0.0 && count <= i64::MAX as f64,
                "a fixed step of {seconds} s"
            );
            Ok(structure([
                ("m_Count", Value::I64(count as i64)),
                (
                    "m_Rate",
                    structure([
                        ("m_Denominator", Value::U32(1)),
                        ("m_Numerator", Value::U32(RATE)),
                    ]),
                ),
            ]))
        },
        folds: &[],
    },
    // A streamed clip's keys all live in one buffer, counted by `curveCount`. Unity 2022
    // split that count: PPtr curves (sprite swaps) moved into `discreteCurveCount`, whose
    // keys follow the continuous ones in the same buffer. The older form counts those
    // same keys in `curveCount` and reads them the same way, so folding the counts
    // together is what the buffer already says — and without it the older player reads
    // keys whose curve index is past `curveCount` and dies once an Animator evaluates
    // the clip.
    FieldRule {
        owner: "StreamedClip",
        field: "curveCount",
        when: |source, target| {
            has_field(source, "discreteCurveCount") && !has_field(target, "discreteCurveCount")
        },
        value: |source| {
            let count = |name| -> Result<u64> {
                match source.field(name)? {
                    Some(Value::U16(n)) => Ok(n.into()),
                    Some(Value::U32(n)) => Ok(n.into()),
                    other => bail!("a streamed clip's {name} is {other:?}"),
                }
            };
            Ok(Value::U32(u32::try_from(
                count("curveCount")? + count("discreteCurveCount")?,
            )?))
        },
        folds: &["discreteCurveCount"],
    },
    // The engine version the build was made with: every build measured (2020.2, 2022.3,
    // 6000.0) writes its own version here, so it follows the target like the file
    // headers do.
    FieldRule {
        owner: "BuildSettings",
        field: "m_Version",
        when: |_, _| true,
        value: |source| Ok(Value::String(source.target().to_string())),
        folds: &[],
    },
    // A mixer's parameters live in one value space that every snapshot materializes:
    // the groups' send slots (`GroupConstant.sendIndex` in ABSENT) are new slots at its
    // end, so every index already in the mixer — the other groups', the effects', the
    // exposed parameters' — keeps pointing where it did. Their value is the level
    // Unity's own builds write for a group that sends nowhere: -80 dB, silence.
    FieldRule {
        owner: "AudioMixerConstant",
        field: "snapshots",
        when: |source, target| {
            let sends = |tree: &TypeTreeNode| {
                child(tree, "groups")
                    .and_then(crate::layout::element)
                    .map(|group| has_field(group, "sendIndex"))
            };
            sends(source) == Some(false) && sends(target) == Some(true)
        },
        value: |source| {
            let Some(Value::Seq(groups)) = source.field("groups")? else {
                bail!("a mixer without groups");
            };
            let Some(Value::Seq(mut snapshots)) = source.field("snapshots")? else {
                bail!("a mixer without snapshots");
            };
            for snapshot in &mut snapshots {
                let Some(Value::Seq(values)) = crate::value::field_mut(snapshot, "values") else {
                    bail!("a mixer snapshot without values");
                };
                values.extend(std::iter::repeat_n(Value::F32(SILENCE_DB), groups.len()));
            }
            Ok(Value::Seq(snapshots))
        },
        folds: &[],
    },
];

/// A GUID made from `text`, the same for the same text: FNV-1a, twice with different
/// offsets for the four words.
fn guid_of(text: &str) -> Value {
    let fnv = |offset: u64| {
        text.bytes().fold(offset, |hash, byte| {
            (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3)
        })
    };
    let (high, low) = (fnv(0xcbf2_9ce4_8422_2325), fnv(0x84222325_cbf29ce4));
    structure([
        ("data[0]", Value::U32((high >> 32) as u32)),
        ("data[1]", Value::U32(high as u32)),
        ("data[2]", Value::U32((low >> 32) as u32)),
        ("data[3]", Value::U32(low as u32)),
    ])
}

/// A struct value of `fields`, in their order.
fn structure<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Map(
        fields
            .into_iter()
            .map(|(name, value)| (Value::String(name.to_owned()), value))
            .collect(),
    )
}

/// The level of a send that is not routed anywhere, as Unity's own mixers store it.
const SILENCE_DB: f32 = -80.0;

/// How many values each snapshot of the mixer `object` holds: the slots of its value
/// space. Snapshots of different sizes would make an index mean different things.
fn snapshot_size(object: &Value) -> Result<usize> {
    let Some(Value::Seq(snapshots)) =
        field(object, "m_MixerConstant").and_then(|constant| field(constant, "snapshots"))
    else {
        bail!("a mixer without snapshots");
    };
    let mut sizes = snapshots
        .iter()
        .map(|snapshot| match field(snapshot, "values") {
            Some(Value::Seq(values)) => Ok(values.len()),
            _ => bail!("a mixer snapshot without values"),
        });
    let first = sizes
        .next()
        .context("a mixer with groups but no snapshots")??;
    for size in sizes {
        let size = size?;
        anyhow::ensure!(
            size == first,
            "mixer snapshots of different sizes: {first} and {size}"
        );
    }
    Ok(first)
}

fn child<'a>(tree: &'a TypeTreeNode, name: &str) -> Option<&'a TypeTreeNode> {
    tree.children.iter().find(|child| child.m_Name == name)
}

/// How a rule decides one field of the target.
#[derive(Clone, Copy)]
pub enum Rule {
    /// A value made from the source object, and the source fields the target lacks that
    /// it carries.
    Value(fn(&dyn Source) -> Result<Value>, &'static [&'static str]),
    /// The source field of the other name, converted.
    Renamed {
        from: &'static str,
        to: &'static str,
        shape: Shape,
        /// Whether `from` is the newer name.
        from_new: bool,
    },
}

impl Rule {
    /// The field's value in the target.
    pub fn value(&self, source: &dyn Source) -> Result<Value> {
        match *self {
            Rule::Value(value, _) => value(source),
            Rule::Renamed {
                from,
                to,
                shape,
                from_new,
            } => {
                let value = source.field(from)?.with_context(|| format!("no {from}"))?;
                convert_renamed(shape, from_new, from, to, value)
            }
        }
    }

    /// The source fields the target lacks that the rule carries, which are not dropped.
    pub fn replaces(&self) -> &'static [&'static str] {
        match *self {
            Rule::Renamed { from, .. } => {
                match CHANGES.iter().find(|c| c.new == from || c.old == from) {
                    Some(change) if change.new == from => std::slice::from_ref(&change.new),
                    Some(change) => std::slice::from_ref(&change.old),
                    None => &[],
                }
            }
            Rule::Value(_, folds) => folds,
        }
    }
}

/// The rule deciding `field` of the target struct `target`, coming from the source
/// struct `source`; `None` where the field carries over from the source field of its
/// name, or where nothing does.
pub fn rule(source: &TypeTreeNode, target: &TypeTreeNode, field: &str) -> Option<Rule> {
    if let Some(found) = FIELD_RULES.iter().find(|rule| {
        rule.owner == target.m_Type && rule.field == field && (rule.when)(source, target)
    }) {
        return Some(Rule::Value(found.value, found.folds));
    }
    if has_field(source, field) {
        return None;
    }
    if let Some(found) = ABSENT
        .iter()
        .find(|absent| absent.owners.contains(&target.m_Type.as_str()) && absent.field == field)
    {
        let _ = found.evidence;
        return Some(Rule::Value(found.value, &[]));
    }
    CHANGES.iter().find_map(|change| {
        let (from, to, from_new) = match field {
            _ if field == change.old => (change.new, change.old, true),
            _ if field == change.new => (change.old, change.new, false),
            _ => return None,
        };
        has_field(source, from).then_some(Rule::Renamed {
            from,
            to,
            shape: change.shape,
            from_new,
        })
    })
}

fn convert_renamed(
    shape: Shape,
    from_new: bool,
    from: &str,
    to: &str,
    value: Value,
) -> Result<Value> {
    Ok(match (shape, from_new) {
        (Shape::Same, _) => value,
        (Shape::BoolAsEnum { set }, true) => match as_int(from, &value)? {
            0 => Value::Bool(false),
            mode if mode == set => Value::Bool(true),
            mode => bail!("{from} is {mode}, which {to} cannot say"),
        },
        (Shape::BoolAsEnum { set }, false) => match value {
            Value::Bool(true) => Value::I32(set as i32),
            Value::Bool(false) => Value::I32(0),
            other => bail!("{from} is {other:?}, not the bool it is in this form"),
        },
        (Shape::BoolAsTwoOf { on, off }, true) => match as_int(from, &value)? {
            mode if mode == on => Value::Bool(true),
            mode if mode == off => Value::Bool(false),
            mode => bail!("{from} is {mode}, which {to} cannot say"),
        },
        (Shape::BoolAsTwoOf { on, off }, false) => match value {
            Value::Bool(true) => Value::I32(on as i32),
            Value::Bool(false) => Value::I32(off as i32),
            other => bail!("{from} is {other:?}, not the bool it is in this form"),
        },
    })
}

fn as_int(field: &str, value: &Value) -> Result<i64> {
    // the leaves carry the width the tree gave them
    match value {
        Value::I8(int) => Ok(*int as i64),
        Value::I16(int) => Ok(*int as i64),
        Value::I32(int) => Ok(*int as i64),
        Value::I64(int) => Ok(*int),
        Value::U8(int) => Ok(*int as i64),
        Value::U16(int) => Ok(*int as i64),
        Value::U32(int) => Ok(*int as i64),
        Value::U64(int) => Ok(*int as i64),
        other => bail!("{field} is {other:?}, not the enum it is in this form"),
    }
}

/// A field Unity's build writes into an object that no type tree of the class names,
/// neither the editor's nor the player's.
struct BuildOnlyField {
    class: &'static str,
    field: &'static str,
    /// A plain type, written without alignment of its own.
    ty: &'static str,
    /// The tree field it is written in front of.
    before: &'static str,
    evidence: &'static str,
}

const BUILD_ONLY_FIELDS: &[BuildOnlyField] = &[
    // The 6000.0 editor's PlayerSettings::Transfer<StreamedBinaryWrite>, when writing for
    // a build target, asks EditorOnlyPlayerSettings::GetCaptureStartupLogs for the
    // target and writes the answer as one byte between insecureHttpOption and the
    // Vulkan filter lists. Its GenerateTypeTreeTransfer, and the player's, leave it
    // out, but the player reads it: the 6000.0.50f1 player, given PlayerSettings
    // without the byte, reads past the object in its filter lists
    // (StreamedBinaryRead::TransferSTLStyleArray<AndroidDeviceFilterData>).
    BuildOnlyField {
        class: "PlayerSettings",
        field: "captureStartupLogs",
        ty: "bool",
        before: "androidVulkanDenyFilterList",
        evidence: "disassembly of the 6000.0.61f1 editor; Silksong 6000.0.50f1 and Hollow \
                   Knight 6000.0.61f1 read exactly with it; the 6000.0.50f1 player reads it",
    },
];

/// The tree of `tree`'s class as a build writes it: with the fields of
/// [`BUILD_ONLY_FIELDS`] it lacks in their place. `None` if none applies.
pub fn build_tree(tree: &TypeTreeNode) -> Option<TypeTreeNode> {
    if !BUILD_ONLY_FIELDS
        .iter()
        .any(|extra| extra.class == tree.m_Type)
    {
        return None;
    }
    let mut patched = tree.clone();
    let mut changed = false;
    for extra in BUILD_ONLY_FIELDS
        .iter()
        .filter(|extra| extra.class == tree.m_Type)
    {
        let _ = extra.evidence;
        if has_field(&patched, extra.field) {
            continue;
        }
        let Some(at) = patched
            .children
            .iter()
            .position(|c| c.m_Name == extra.before)
        else {
            continue;
        };
        let template = &patched.children[at];
        patched.children.insert(
            at,
            TypeTreeNode {
                m_Type: extra.ty.into(),
                m_Name: extra.field.into(),
                m_ByteSize: 1,
                m_Version: 1,
                m_MetaFlag: Some(0),
                ..template.clone()
            }
            .without_children(),
        );
        changed = true;
    }
    changed.then_some(patched)
}

trait WithoutChildren {
    fn without_children(self) -> Self;
}

impl WithoutChildren for TypeTreeNode {
    fn without_children(mut self) -> Self {
        self.children.clear();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabex::serde_typetree;

    fn node(ty: &str, name: &str, align: bool, children: Vec<TypeTreeNode>) -> TypeTreeNode {
        TypeTreeNode {
            m_Type: ty.into(),
            m_Name: name.into(),
            m_MetaFlag: Some(if align { 0x4000 } else { 0 }),
            children,
            ..Default::default()
        }
    }

    /// The tail of 6000.0's PlayerSettings tree: an int and two vectors of strings.
    fn player_settings_tail() -> TypeTreeNode {
        let list = |name: &str| {
            node(
                "vector",
                name,
                false,
                vec![node(
                    "Array",
                    "Array",
                    true,
                    vec![
                        node("int", "size", false, vec![]),
                        node("string", "data", false, vec![]),
                    ],
                )],
            )
        };
        node(
            "PlayerSettings",
            "Base",
            false,
            vec![
                node("int", "insecureHttpOption", false, vec![]),
                list("androidVulkanDenyFilterList"),
                list("androidVulkanAllowFilterList"),
            ],
        )
    }

    #[test]
    fn a_build_only_field_reads_where_the_build_wrote_it() {
        // what the build writes with captureStartupLogs on: the byte, then the lists'
        // sizes, each list aligned after itself
        let bytes = [
            &2_i32.to_le_bytes()[..],
            &[1],
            &0_i32.to_le_bytes(),
            &[0, 0, 0],
            &0_i32.to_le_bytes(),
        ]
        .concat();
        let tree = player_settings_tail();
        // the tree alone takes the byte for the first list's size: it reads to the end
        // as well, as a list holding one empty string — the reason a reader has to see
        // both forms
        let mut cursor = std::io::Cursor::new(bytes.as_slice());
        let misread: Value =
            serde_typetree::from_reader::<_, byteorder::LittleEndian>(&mut cursor, &tree).unwrap();
        assert_eq!(cursor.position(), bytes.len() as u64);
        assert_eq!(
            field(&misread, "androidVulkanDenyFilterList"),
            Some(&Value::Seq(vec![Value::String(String::new())]))
        );

        let build = build_tree(&tree).expect("the field applies");
        let mut cursor = std::io::Cursor::new(bytes.as_slice());
        let value: Value =
            serde_typetree::from_reader::<_, byteorder::LittleEndian>(&mut cursor, &build).unwrap();
        assert_eq!(cursor.position(), bytes.len() as u64);
        assert_eq!(
            field(&value, "captureStartupLogs"),
            Some(&Value::Bool(true))
        );
        assert_eq!(field(&value, "insecureHttpOption"), Some(&Value::I32(2)));
    }
}
