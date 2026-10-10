"""openpyxl-compatible ``Image`` data holder for ``ws.add_image(img, "E5")``.

Holds the raw bytes plus size/anchor; the Rust engine detects the format,
reads the pixel size and writes all package parts.
"""

from __future__ import annotations

import os
from typing import IO

from kyrax._kyrax import image_size as _image_size


class Image:
    """An image to place on a worksheet (PNG, JPEG or GIF).

    ``width`` / ``height`` are pixels (read from the file header; may be
    changed before ``add_image``). ``anchor`` is the top-left cell.
    """

    def __init__(self, img: str | os.PathLike[str] | IO[bytes]) -> None:
        if hasattr(img, "read"):
            self._data: bytes = img.read()  # type: ignore[union-attr]
        else:
            with open(img, "rb") as fh:  # type: ignore[arg-type]
                self._data = fh.read()
        self.width, self.height = _image_size(self._data)
        self.anchor: str = "A1"
