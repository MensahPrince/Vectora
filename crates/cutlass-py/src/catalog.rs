//! Effect, transition, and sticker catalog introspection.

use cutlass_models::{effect_catalog, sticker_catalog, transition_catalog};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::convert::param_spec_dict;

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(effects, m)?)?;
    m.add_function(wrap_pyfunction!(transitions, m)?)?;
    m.add_function(wrap_pyfunction!(stickers, m)?)?;
    Ok(())
}

#[pyfunction]
fn effects(py: Python) -> PyResult<Vec<Py<PyDict>>> {
    effect_catalog()
        .iter()
        .map(|spec| {
            let dict = PyDict::new(py);
            dict.set_item("id", spec.id)?;
            dict.set_item("label", spec.label)?;
            let params: Vec<Py<PyDict>> = spec
                .params
                .iter()
                .map(|p| param_spec_dict(py, p.name, p.label, p.default, p.min, p.max))
                .collect::<PyResult<_>>()?;
            dict.set_item("params", params)?;
            Ok(dict.into())
        })
        .collect()
}

#[pyfunction]
fn stickers(py: Python) -> PyResult<Vec<Py<PyDict>>> {
    sticker_catalog()
        .iter()
        .map(|spec| {
            let dict = PyDict::new(py);
            dict.set_item("id", spec.id)?;
            dict.set_item("label", spec.label)?;
            dict.set_item("width", spec.width)?;
            dict.set_item("height", spec.height)?;
            dict.set_item("animated", spec.animated)?;
            Ok(dict.into())
        })
        .collect()
}

#[pyfunction]
fn transitions(py: Python) -> PyResult<Vec<Py<PyDict>>> {
    transition_catalog()
        .iter()
        .map(|spec| {
            let dict = PyDict::new(py);
            dict.set_item("id", spec.id)?;
            dict.set_item("label", spec.label)?;
            Ok(dict.into())
        })
        .collect()
}
