use super::error;
use crate::value::NumericArray;
use nalgebra::{DMatrix, DVector};

pub(super) fn matrix_multiply(
    left: &NumericArray,
    right: &NumericArray,
) -> Result<NumericArray, String> {
    if left.cols != right.rows {
        return Err(error("矩阵乘法维度不匹配"));
    }
    let left = DMatrix::from_row_slice(left.rows, left.cols, &left.data);
    let right = DMatrix::from_row_slice(right.rows, right.cols, &right.data);
    let result = left * right;
    let mut data = Vec::with_capacity(result.len());
    for row in 0..result.nrows() {
        for column in 0..result.ncols() {
            data.push(result[(row, column)]);
        }
    }
    Ok(NumericArray {
        data,
        rows: result.nrows(),
        cols: result.ncols(),
    })
}

pub(super) fn solve(matrix: &NumericArray, vector: &NumericArray) -> Result<NumericArray, String> {
    if matrix.rows != matrix.cols || vector.data.len() != matrix.rows {
        return Err(error("线性方程维度不匹配"));
    }
    let matrix = DMatrix::from_row_slice(matrix.rows, matrix.cols, &matrix.data);
    let vector = DVector::from_vec(vector.data.clone());
    let result = matrix
        .lu()
        .solve(&vector)
        .ok_or_else(|| error("矩阵不可逆，无法求解"))?;
    Ok(NumericArray {
        data: result.iter().copied().collect(),
        rows: result.len(),
        cols: 1,
    })
}

pub(super) fn eigenvalues(matrix: &NumericArray) -> Result<NumericArray, String> {
    if matrix.rows != matrix.cols {
        return Err(error("特征值要求方阵"));
    }
    let matrix = DMatrix::from_row_slice(matrix.rows, matrix.cols, &matrix.data);
    let values = matrix.complex_eigenvalues();
    Ok(NumericArray {
        data: values.iter().map(|value| value.re).collect(),
        rows: values.len(),
        cols: 1,
    })
}

pub(super) fn singular_values(matrix: &NumericArray) -> Result<NumericArray, String> {
    let matrix = DMatrix::from_row_slice(matrix.rows, matrix.cols, &matrix.data);
    let values = matrix.svd(false, false).singular_values;
    Ok(NumericArray {
        data: values.iter().copied().collect(),
        rows: values.len(),
        cols: 1,
    })
}
