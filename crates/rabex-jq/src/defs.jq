def maybe(f): if . != null then f else null end;
def nonnull: select(. != null);
def filterkeys(text): with_entries(select(.key | contains(text)));

def go: .m_GameObject | deref;
def name: if .m_Name != "" then .m_Name else go | .m_Name end;

# monobehaviour
def script_name: .m_Script | deref | .m_ClassName;

# game object
def components: .m_Component[].component;
def components(class_id): components | select(.class_id == class_id) | deref;
def scripts: components("MonoBehaviour");
def transform: components("Transform");
def scripts(name): components("MonoBehaviour") | select(script_name == name);

# vector / quaternion math (Vector3 {x,y,z}, Quaternion {x,y,z,w})
def v3add($a; $b): {x: ($a.x + $b.x), y: ($a.y + $b.y), z: ($a.z + $b.z)};
def v3mul($a; $b): {x: ($a.x * $b.x), y: ($a.y * $b.y), z: ($a.z * $b.z)};
def v3scale($v; $s): {x: ($v.x * $s), y: ($v.y * $s), z: ($v.z * $s)};
def cross($a; $b):
    { x: ($a.y * $b.z - $a.z * $b.y),
      y: ($a.z * $b.x - $a.x * $b.z),
      z: ($a.x * $b.y - $a.y * $b.x) };
# rotate vector $v by quaternion $q: v + 2w(u×v) + 2(u×(u×v)), u = q.xyz
def qrot($q; $v):
    {x: $q.x, y: $q.y, z: $q.z} as $u
    | cross($u; $v) as $uv
    | v3add($v; v3add(v3scale($uv; 2 * $q.w); v3scale(cross($u; $uv); 2)));
# quaternion product $a*$b
def qmul($a; $b):
    { x: ($a.w*$b.x + $a.x*$b.w + $a.y*$b.z - $a.z*$b.y),
      y: ($a.w*$b.y - $a.x*$b.z + $a.y*$b.w + $a.z*$b.x),
      z: ($a.w*$b.z + $a.x*$b.y - $a.y*$b.x + $a.z*$b.w),
      w: ($a.w*$b.w - $a.x*$b.x - $a.y*$b.y - $a.z*$b.z) };

# transforms
def parent: transform | .m_Father | maybe(deref) | maybe(go);

# world-space transform: fold the local TRS up the m_Father chain. Input is a Transform (reach it
# with `go | transform` from a component). `world_position` is the common shortcut.
def world_transform:
    .m_LocalPosition as $lp | .m_LocalRotation as $lq | .m_LocalScale as $ls | .m_Father as $f
    | if $f == null or ($f.path_id // 0) == 0 then {pos: $lp, rot: $lq, scale: $ls}
      else ($f | deref | world_transform) as $p
        | { pos:   v3add($p.pos; qrot($p.rot; v3mul($p.scale; $lp))),
            rot:   qmul($p.rot; $lq),
            scale: v3mul($p.scale; $ls) }
      end;
def world_position: world_transform | .pos;
def path_components: parent as $parent |
    if $parent == null then [name]
    else ($parent | path_components) + [name]
    end;
def path: parent as $parent |
    if $parent == null then name
    else ($parent | path) + "/" + name
    end;

def fsm: scripts("PlayMakerFSM");

def depth1: del(.[]?[]?);
def depth2: del(.[]?[]?[]?);
def depth3: del(.[]?[]?[]?[]?);
