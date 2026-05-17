use pyo3::prelude::*;

mod analysis;
mod capture;
mod engine;

/// High-performance cost bar analysis engine
#[pymodule]
fn ruler_rust(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<engine::RulerEngine>()?;
    m.add_class::<engine::PyFrameResult>()?;
    Ok(())
}
