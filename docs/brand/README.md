# Brand files

The bridlework logo. The colours are `#2cd6ca` (mane and tick), `#f7a13b` (bridle and shield), `#fffaf0` (head and lettering on a dark ground), `#0b1220` (head and lettering on a light ground) and `#080d18` (the dark ground).

| file | use it for | do not use it for |
|---|---|---|
| `bridlework-logo.svg` | dark backgrounds: dark pages, slides, terminal-themed pages | white or light backgrounds: the head and the lettering are cream and disappear |
| `bridlework-logo-light.svg` | white or light backgrounds: print, light pages | dark backgrounds: the head and the lettering are dark navy and disappear |
| `bridlework-mark.svg` | small sizes and squares: favicon, avatar, app icon | places with room for the full logo. In a render check the full logo's lettering stayed readable down to about 80 px wide and blurred near 60 px; below that, and for any square slot, use the mark |
| `logo-preview.html` | a page to look at the logo on its dark ground and save the SVG | an image: it is a viewer page, so it cannot be used in an `<img>` or in the README |

`bridlework-logo.svg` and `logo-preview.html` are the supplied files, unchanged. The builder derived the other two from `bridlework-logo.svg`: `bridlework-logo-light.svg` swaps the fill `#fffaf0` for `#0b1220`; `bridlework-mark.svg` drops the lettering subpaths, crops the viewBox to a square and sets the mark on a rounded `#080d18` square.

The top of the repository README shows `bridlework-logo.svg` to readers on a dark theme and `bridlework-logo-light.svg` to everyone else.
