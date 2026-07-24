import argparse
from enum import Enum

from .readers import kyrax_read, pyxl_read, xlrd_read


class Engine(str, Enum):
    KYRAX = "kyrax"
    XLRD = "xlrd"
    OPENPYXL = "pyxl"


def get_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("-e", "--engine", default=Engine.KYRAX)
    parser.add_argument("file")
    return parser.parse_args()


def main():
    args = get_args()
    engine = args.engine

    if engine == Engine.KYRAX:
        kyrax_read(args.file)
    elif engine == Engine.XLRD:
        xlrd_read(args.file)
    elif engine == Engine.OPENPYXL:
        pyxl_read(args.file)


if __name__ == "__main__":
    main()
