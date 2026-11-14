# Image Plugin for Schrödinger

Clean up, convert or shrink an image on its way to the paste.

- **Remove metadata**: drops location (GPS), camera and lens details, timestamps, embedded thumbnails, editing history and comments
- **As PNG** / **As JPEG**: convert
- **Resize**: fit within a size you choose in the plugin's settings
- **Remove background**: keeps the subject (a person, an animal, an object) on a transparent background, as a PNG

Works on an image that is on the clipboard as pixels (a screenshot, an image copied from a web page) and on a single image file copied in Finder or Explorer. For a copied file the result is a new file named after the original (`IMG_0042-clean.jpg`), so pasting into Mail or a chat attaches the processed copy. The original file is never changed.

Reads PNG, JPEG, TIFF, GIF, BMP and WebP everywhere. On macOS it also reads HEIC, the iPhone's photo format, and removes backgrounds; both use the operating system's own image support through [loupe](https://github.com/georgemandis/loupe).

## HEIC photos

A copied HEIC photo can be pasted as a JPEG or PNG, resized, or cleaned of metadata. HEIC cannot be cleaned in place, so **Remove metadata** and **Resize** give back a JPEG. The photo is drawn upright first, and colors are converted to sRGB.

## Removing the background

Uses the same subject detection as "Copy Subject" in Photos and Preview. It needs macOS 14 or later. When nothing stands out from the background, the paste reports that instead of producing an empty image. The edge is soft (partly transparent pixels), which looks right on any background.

## How metadata is removed

For JPEG and PNG the metadata sits in its own blocks next to the compressed image, so the plugin copies the file and leaves those blocks out. The image data is not decoded or re-compressed, and there is no quality loss.

Two things are kept on purpose: the color profile, because removing it changes how the colors look, and the markers a decoder needs.

One exception: a photo stored sideways relies on a metadata tag to be shown upright. Removing that tag would leave it rotated, so such photos are redrawn upright and saved once at high quality.

Other formats are re-encoded as PNG, which keeps every pixel and carries no metadata over.

## Settings

- **Resize to fit within**: longest side in pixels (640, 1024, 1600, 2048 or 3840). Images already smaller are left alone.
- **JPEG quality**: used whenever a JPEG has to be re-encoded (Low, Medium, High, Maximum)
- **After removing the background**: trim the result to the subject (default), or keep the original size

## Limits

- One image at a time: several copied files are not handled yet.
- HEIC and background removal are macOS only. On Windows and Linux the **Remove background** format is listed but reports that it is unavailable.
- Converting to JPEG puts transparent areas on white.

## Building

```sh
cargo test
sh scripts/package-macos.sh   # builds, then stages dist/image/ ready to install
```

The package script matters on macOS. The plugin links loupe as a dynamic library, and so do other Schrödinger plugins (OCR, Faces), each bundling its own copy. All plugins load into one process, and macOS matches an already-loaded library by its install name, so under a shared name one plugin can be handed another's older copy and fail to load. The script gives this plugin's copy its own name (`loupe-img.dylib`).

To run every format on a real file:

```sh
IMAGE_FILE=/path/to/photo.heic OUT_DIR_IMAGES=/tmp/out cargo test --release real_file -- --ignored --nocapture
```

## License

MIT
