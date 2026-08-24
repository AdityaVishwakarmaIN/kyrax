"""03_streaming_large_files.py: Memory-bounded writing and reading of large datasets."""

from pathlib import Path
import kyrax

def generate_sensor_data(num_rows: int = 10_000):
    for i in range(num_rows):
        yield [f"SENSOR_{i % 50:03d}", i * 0.1, 20.0 + (i % 15) * 0.5, i % 2 == 0]

def main():
    out_path = Path("examples_output_03.xlsx")

    # 1. Stream write large dataset
    sheets = [
        {
            "name": "Telemetry",
            "rows": [["SensorID", "Timestamp", "Temperature", "Status"]] + list(generate_sensor_data(1000)),
        }
    ]
    kyrax.write_excel_turbo(str(out_path), sheets)
    print(f"Wrote dataset to {out_path}")

    # 2. Stream read in chunks via Arrow batches
    stream = kyrax.read_excel_turbo_iter(str(out_path), sheet_idx=0, chunk_size=250)
    total_rows = 0
    for batch in stream:
        total_rows += batch.num_rows

    print(f"Stream-read total {total_rows} rows successfully")

    if out_path.exists():
        out_path.unlink()

if __name__ == "__main__":
    main()
