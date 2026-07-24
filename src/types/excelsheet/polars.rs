use crate::{KyraxColumn, KyraxSeries};
use polars_core::{
    frame::column::{Column as PolarsColumn, ScalarColumn},
    prelude::DataType,
    scalar::Scalar,
};

impl From<KyraxColumn> for PolarsColumn {
    fn from(column: KyraxColumn) -> Self {
        let name = column.name().into();
        match column.data {
            KyraxSeries::Null => PolarsColumn::Scalar(ScalarColumn::new(
                name,
                Scalar::null(DataType::Null),
                column.len(),
            )),
            KyraxSeries::Bool(values) => PolarsColumn::new(name, values),
            KyraxSeries::String(values) => PolarsColumn::new(name, values),
            KyraxSeries::Int(values) => PolarsColumn::new(name, values),
            KyraxSeries::Float(values) => PolarsColumn::new(name, values),
            KyraxSeries::Datetime(values) => PolarsColumn::new(name, values),
            KyraxSeries::Date(values) => PolarsColumn::new(name, values),
            KyraxSeries::Duration(values) => PolarsColumn::new(name, values),
        }
    }
}
