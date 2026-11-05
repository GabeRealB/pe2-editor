//! What a 3D view counts with.

pub type V3 = [f32; 3];
pub type M3 = [[f32; 3]; 3];
/// By columns, as the shaders read it.
pub type M4 = [[f32; 4]; 4];

pub const IDENTITY: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn scale(a: V3, by: f32) -> V3 {
    [a[0] * by, a[1] * by, a[2] * by]
}

pub fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub fn length(a: V3) -> f32 {
    dot(a, a).sqrt()
}

pub fn normalized(a: V3) -> V3 {
    let size = length(a);
    if size > 0.0 { scale(a, 1.0 / size) } else { a }
}

/// Rows, as the game stores them: `rotate(m, v)` is m v.
pub fn rotate(m: &M3, v: V3) -> V3 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}

pub fn compose(a: &M3, b: &M3) -> M3 {
    let column = |c: usize| [b[0][c], b[1][c], b[2][c]];
    std::array::from_fn(|r| std::array::from_fn(|c| dot(a[r], column(c))))
}

pub fn transposed(m: &M3) -> M3 {
    std::array::from_fn(|r| std::array::from_fn(|c| m[c][r]))
}

/// A turn of `angle` radians about a unit axis.
pub fn turn(axis: V3, angle: f32) -> M3 {
    let (sine, cosine) = angle.sin_cos();
    let [x, y, z] = axis;
    let rest = 1.0 - cosine;
    [
        [rest * x * x + cosine, rest * x * y - sine * z, rest * x * z + sine * y],
        [rest * x * y + sine * z, rest * y * y + cosine, rest * y * z - sine * x],
        [rest * x * z - sine * y, rest * y * z + sine * x, rest * z * z + cosine],
    ]
}

pub fn multiply(a: &M4, b: &M4) -> M4 {
    std::array::from_fn(|column| std::array::from_fn(|row| (0..4).map(|k| a[k][row] * b[column][k]).sum()))
}

pub fn transform(m: &M4, v: V3) -> [f32; 4] {
    std::array::from_fn(|row| m[0][row] * v[0] + m[1][row] * v[1] + m[2][row] * v[2] + m[3][row])
}

pub fn m4_bytes(m: &M4) -> Vec<u8> {
    m.iter().flatten().flat_map(|value| value.to_le_bytes()).collect()
}

/// The inverse of a matrix, none where it flattens something.
#[must_use]
pub fn inverse(matrix: &M3) -> Option<M3> {
    let cofactor = |row: usize, column: usize| {
        let (below, under) = ((row + 1) % 3, (row + 2) % 3);
        let (next, last) = ((column + 1) % 3, (column + 2) % 3);
        matrix[below][next] * matrix[under][last] - matrix[below][last] * matrix[under][next]
    };
    let determinant: f32 = (0..3).map(|column| matrix[0][column] * cofactor(0, column)).sum();
    (determinant.abs() > 1e-9)
        .then(|| std::array::from_fn(|row| std::array::from_fn(|column| cofactor(column, row) / determinant)))
}

#[cfg(test)]
mod inverting {
    use super::*;

    #[test]
    fn a_matrix_and_its_inverse_undo_each_other() {
        let matrix = [[2.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 2.0]];
        let product = compose(&matrix, &inverse(&matrix).unwrap());
        for (row, line) in product.iter().enumerate() {
            for (column, value) in line.iter().enumerate() {
                assert!((value - f32::from(u8::from(row == column))).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn what_flattens_has_no_inverse() {
        assert!(inverse(&[[1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]]).is_none());
    }
}
