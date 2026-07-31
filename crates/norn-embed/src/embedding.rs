//! A vector and the model that produced it.

use crate::model::Model;

/// One embedding: the values, carrying the identity of the model they came
/// from.
///
/// **The model travels with the vector.** A caller that holds an `Embedding`
/// never has to remember which embedder it asked, which is what keeps
/// `(model_id, model_version)` correct in derived state that outlives the
/// request.
///
/// The values are `f32` and nothing here says how they are stored. Element
/// width, quantization and blob layout are the storage side's mechanics; this
/// type is the numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct Embedding {
    model: Model,
    values: Vec<f32>,
}

impl Embedding {
    /// An embedding of `values`, produced by `model`.
    pub fn new(model: Model, values: Vec<f32>) -> Self {
        Embedding { model, values }
    }

    /// The model that produced it.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The values, in the order the model produced them.
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// How many values there are. Equal to the producing embedder's
    /// [`dimensions`](crate::Embedder::dimensions).
    pub fn dimensions(&self) -> usize {
        self.values.len()
    }
}
