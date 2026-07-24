#[allow(unused_macros)]
mod utils;

use anyhow::{Context, Result};
use kyrax::LoadSheetOrTableOptions;
use pretty_assertions::assert_eq;
use utils::path_for_fixture;

#[test]
fn test_sheet_with_offset() -> Result<()> {
    let mut reader = kyrax::read_excel(path_for_fixture("sheet-and-table-with-offset.xlsx"))
        .context("could not read the excel file")?;
    let mut sheet = reader
        .load_sheet(
            "without-table".into(),
            LoadSheetOrTableOptions::new_for_sheet(),
        )
        .context("could not load sheet \"without-table\"")?;

    let available_columns = sheet
        .available_columns()
        .context("could not obtain available columns for sheet")?;
    let expected_column_info = vec![
        kyrax::ColumnInfo {
            name: "Column at H10".into(),
            index: 0,
            absolute_index: 7,
            dtype: kyrax::DType::Float,
            dtype_from: kyrax::DTypeFrom::Guessed,
            column_name_from: kyrax::ColumnNameFrom::LookedUp,
        },
        kyrax::ColumnInfo {
            name: "Column at I10".into(),
            index: 1,
            absolute_index: 8,
            dtype: kyrax::DType::Float,
            dtype_from: kyrax::DTypeFrom::Guessed,
            column_name_from: kyrax::ColumnNameFrom::LookedUp,
        },
        kyrax::ColumnInfo {
            name: "__UNNAMED__2".into(),
            index: 2,
            absolute_index: 9,
            dtype: kyrax::DType::String,
            dtype_from: kyrax::DTypeFrom::Guessed,
            column_name_from: kyrax::ColumnNameFrom::Generated,
        },
        kyrax::ColumnInfo {
            name: "Column at K10".into(),
            index: 3,
            absolute_index: 10,
            dtype: kyrax::DType::Float,
            dtype_from: kyrax::DTypeFrom::Guessed,
            column_name_from: kyrax::ColumnNameFrom::LookedUp,
        },
    ];
    assert_eq!(available_columns, expected_column_info);

    Ok(())
}

#[test]
fn test_table_with_offset() -> Result<()> {
    let mut reader = kyrax::read_excel(path_for_fixture("sheet-and-table-with-offset.xlsx"))
        .context("could not read the excel file")?;
    let mut table = reader
        .load_table("TableAtD5", LoadSheetOrTableOptions::new_for_table())
        .context("could not load table \"TableAtD5\"")?;

    let available_columns = table
        .available_columns()
        .context("could not obtain available columns for table")?;
    let expected_column_info = vec![
        kyrax::ColumnInfo {
            name: "Column at D5".into(),
            index: 0,
            absolute_index: 3,
            dtype: kyrax::DType::Float,
            dtype_from: kyrax::DTypeFrom::Guessed,
            column_name_from: kyrax::ColumnNameFrom::Provided,
        },
        kyrax::ColumnInfo {
            name: "Column at E5".into(),
            index: 1,
            absolute_index: 4,
            dtype: kyrax::DType::Float,
            dtype_from: kyrax::DTypeFrom::Guessed,
            column_name_from: kyrax::ColumnNameFrom::Provided,
        },
    ];
    assert_eq!(available_columns, expected_column_info);

    Ok(())
}
