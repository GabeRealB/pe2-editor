//! A skeleton: the joints a character's meshes hang from and its animations move, by name, each where it rests under
//! its parent. It is shared, so meshes hold vertices alone; the game's nodes are all rigid, so this is the whole of it.

use pe2_ext::{Asset, Result, Value, anyhow};

#[derive(Clone, Debug, PartialEq)]
pub struct Joint {
    pub name: String,
    pub parent: Option<usize>,
    pub translation: [i32; 3],
    /// In degrees, the order the game applies them: Rx Ry Rz.
    pub rotation: [f64; 3],
}

fn number(value: &Value) -> Option<f64> {
    #[expect(clippy::cast_precision_loss, reason = "angles")]
    value.as_float().or_else(|| value.as_integer().map(|x| x as f64))
}

fn triple<T: Copy + Default>(value: Option<&Value>, read: impl Fn(&Value) -> Option<T>) -> Option<[T; 3]> {
    let values = value?.as_array()?;
    Some([read(values.first()?)?, read(values.get(1)?)?, read(values.get(2)?)?])
}

/// A transform's translation, in a node's table.
#[must_use]
pub fn translation_of(table: &Value) -> Option<[i32; 3]> {
    triple(table.get("translation"), |x| x.as_integer().and_then(|x| i32::try_from(x).ok()))
}

/// A transform's three numbers in degrees, or as scale: whole or not.
#[must_use]
pub fn numbers_of(table: &Value, key: &str) -> Option<[f64; 3]> {
    triple(table.get(key), number)
}

/// What a prefab says of a joint: it replaces what the skeleton says, as far as it goes.
pub fn posed(part: &mut crate::model::Part, pose: &Value) {
    part.translation = translation_of(pose).unwrap_or(part.translation);
    if let Some(rotation) = numbers_of(pose, "rotation") {
        part.rotation = matrix(rotation);
    }
}

/// How a prefab holds what it attaches: over what the model's root says of itself, as a child of the node.
pub fn held(part: &mut crate::model::Part, child: &Value) {
    let [x, y, z] = translation_of(child).unwrap_or_default();
    part.translation = [part.translation[0] + x, part.translation[1] + y, part.translation[2] + z];
    let turned = numbers_of(child, "rotation").map_or([[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]], matrix);
    let scale = numbers_of(child, "scale").unwrap_or([1.0; 3]);
    let wide = |row: &[i16; 3]| row.map(f64::from);
    let (turned, rest) = (turned.map(|row| wide(&row)), part.rotation.map(|row| wide(&row)));
    #[expect(clippy::cast_possible_truncation, reason = "clamped to what the game's fixed point holds")]
    let cell = |row: usize, column: usize| {
        ((0..3).map(|k| turned[row][k] * scale[k] * rest[k][column]).sum::<f64>() / 4096.0)
            .clamp(-32768.0, 32767.0)
            .round() as i16
    };
    part.rotation = std::array::from_fn(|row| std::array::from_fn(|column| cell(row, column)));
}

/// Joints as a skeleton lists them: `{ name, parent, translation, rotation }`, parents first.
///
/// # Errors
/// One is malformed, or names a parent not before it.
pub fn parse(joints: &[Value]) -> Result<Vec<Joint>> {
    let mut out: Vec<Joint> = Vec::with_capacity(joints.len());
    for joint in joints {
        let name = joint.get("name").and_then(Value::as_str).ok_or_else(|| anyhow!("a joint has no name"))?;
        let parent = match joint.get("parent").and_then(Value::as_str) {
            Some(parent) => {
                Some(out.iter().position(|j| j.name == parent).ok_or_else(|| anyhow!("{name}: no parent {parent}"))?)
            }
            None => None,
        };
        let whole = |x: &Value| x.as_integer().and_then(|x| i32::try_from(x).ok());
        let translation = triple(joint.get("translation"), whole).ok_or_else(|| anyhow!("{name}: no translation"))?;
        let rotation = triple(joint.get("rotation"), number).ok_or_else(|| anyhow!("{name}: no rotation"))?;
        out.push(Joint { name: name.to_string(), parent, translation, rotation });
    }
    Ok(out)
}

/// # Errors
/// See `parse`.
pub fn of(asset: &Asset) -> Result<Vec<Joint>> {
    parse(asset.array("joints"))
}

/// A joint's rotation as the game stores one: 4096 is one, Rx Ry Rz.
#[must_use]
pub fn matrix(rotation: [f64; 3]) -> [[i16; 3]; 3] {
    let [(sx, cx), (sy, cy), (sz, cz)] = rotation.map(|degrees| degrees.to_radians().sin_cos());
    let rows = [
        [cy * cz, -cy * sz, sy],
        [sx * sy * cz + cx * sz, -sx * sy * sz + cx * cz, -sx * cy],
        [-cx * sy * cz + sx * sz, cx * sy * sz + sx * cz, cx * cy],
    ];
    #[expect(clippy::cast_possible_truncation, reason = "a fixed-point rotation is within range")]
    rows.map(|row| row.map(|x| (x * 4096.0).round() as i16))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joint(text: &str) -> Value {
        format!("v = {text}").parse::<pe2_ext::Table>().unwrap().remove("v").unwrap()
    }

    #[test]
    fn parents_are_found_by_name() {
        let joints = parse(&[
            joint(r#"{ name = "root", translation = [0, 0, 0], rotation = [0, 0, 0] }"#),
            joint(r#"{ name = "arm", parent = "root", translation = [1, -2, 3], rotation = [0.0, 90.0, 0] }"#),
        ])
        .unwrap();
        assert_eq!(joints[1].parent, Some(0));
        assert_eq!(joints[1].translation, [1, -2, 3]);
        assert_eq!(joints[1].rotation, [0.0, 90.0, 0.0]);
    }

    #[test]
    fn a_parent_comes_first() {
        assert!(
            parse(&[joint(r#"{ name = "arm", parent = "root", translation = [0, 0, 0], rotation = [0, 0, 0] }"#)])
                .is_err()
        );
    }

    #[test]
    fn no_rotation_is_the_identity() {
        assert_eq!(matrix([0.0; 3]), [[4096, 0, 0], [0, 4096, 0], [0, 0, 4096]]);
    }
}
