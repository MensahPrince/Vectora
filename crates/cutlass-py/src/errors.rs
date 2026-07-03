//! Python exception types and [`ModelError`] mapping.

use cutlass_models::ModelError;
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(cutlass, CutlassError, PyException);
create_exception!(cutlass, OverlapError, CutlassError);
create_exception!(cutlass, TrackKindError, CutlassError);
create_exception!(cutlass, MediaError, CutlassError);
create_exception!(cutlass, RenderError, CutlassError);

/// Register exception types on the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("CutlassError", m.py().get_type::<CutlassError>())?;
    m.add("OverlapError", m.py().get_type::<OverlapError>())?;
    m.add("TrackKindError", m.py().get_type::<TrackKindError>())?;
    m.add("MediaError", m.py().get_type::<MediaError>())?;
    m.add("RenderError", m.py().get_type::<RenderError>())?;
    Ok(())
}

/// Map a model mutation error to the appropriate Python exception.
pub fn model_err(e: ModelError) -> PyErr {
    match e {
        ModelError::Overlap(_) => OverlapError::new_err(e.to_string()),
        ModelError::IncompatibleTrackKind { .. } => TrackKindError::new_err(e.to_string()),
        ModelError::UnknownMedia(_)
        | ModelError::MediaReferenced(_)
        | ModelError::SourceOutOfBounds => MediaError::new_err(e.to_string()),
        _ => CutlassError::new_err(e.to_string()),
    }
}

/// Map any displayable error into [`RenderError`].
pub fn render_err<E: std::fmt::Display>(e: E) -> PyErr {
    RenderError::new_err(e.to_string())
}

/// Map probe / I/O failures into [`MediaError`].
pub fn media_err<E: std::fmt::Display>(e: E) -> PyErr {
    MediaError::new_err(e.to_string())
}
