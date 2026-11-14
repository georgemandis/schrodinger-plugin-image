# Image Plugin for Schrödinger

Clean up, convert or shrink an image on its way to the paste.

- **Remove metadata**: drops location (GPS), camera and lens details, timestamps, embedded thumbnails, editing history and comments
- **As PNG** / **As JPEG**: convert
- **Resize**: fit within a size you choose in the plugin's settings

Works on an image that is on the clipboard as pixels (a screenshot, an image copied from a web page) and on a single image file copied in Finder or Explorer. For a copied file the result is a new file named after the original (`IMG_0042-clean.jpg`), so pasting into Mail or a chat attaches the processed copy. The original file is never changed.

Reads PNG, JPEG, TIFF, GIF, BMP and WebP.

## How metadata is removed

For JPEG and PNG the metadata sits in its own blocks next to the compressed image, so the plugin copies the file and leaves those blocks out. The image data is not decoded or re-compressed, and there is no quality loss.

Two things are kept on purpose: the color profile, because removing it changes how the colors look, and the markers a decoder needs.

One exception: a photo stored sideways relies on a metadata tag to be shown upright. Removing that tag would leave it rotated, so such photos are redrawn upright and saved once at high quality.

Other formats are re-encoded as PNG, which keeps every pixel and carries no metadata over.

## Settings

- **Resize to fit within**: longest side in pixels (640, 1024, 1600, 2048 or 3840). Images already smaller are left alone.
- **JPEG quality**: used whenever a JPEG has to be re-encoded (Low, Medium, High, Maximum)

## Limits

- One image at a time: several copied files are not handled yet.
- HEIC (the iPhone's photo format) is not read yet.
- Converting to JPEG puts transparent areas on white.

## Building

```sh
cargo build --release
cargo test
```

## License

MIT
