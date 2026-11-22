// CRT: curved glass, scanlines, an aperture grille, a little color fringing and dark
// corners. Ghostty's custom-shader format (mainImage, iChannel0 = the terminal screen).
// Static (no clock): the window is only drawn again when something on it changes.
// Each number #define is an adjustment in the Library; the [min, max] above it is its range.

// How flat the glass is, across and down (higher = flatter)
// [4, 60]
#define CURVE_X 14.0
// [4, 60]
#define CURVE_Y 11.0
// Pixels per scanline
// [1.5, 8]
#define SCANLINE_PERIOD 3.0
// How dark the gap between lines gets
// [0, 0.6]
#define SCANLINE_DEPTH 0.16
// How much the grille dims the two channels a column doesn't favor
// [0, 0.5]
#define MASK_DEPTH 0.12
// Pixels between the red and blue channels
// [0, 3]
#define FRINGE 0.6
// How fast the corners darken (lower = only right at the edge)
// [0, 1]
#define VIGNETTE 0.25
// How wide the bright middle is (higher = wider)
// [1, 4]
#define VIGNETTE_PLATEAU 1.6

vec2 curve(vec2 uv) {
    vec2 c = uv * 2.0 - 1.0;
    c *= 1.0 + pow(abs(c.yx) / vec2(CURVE_X, CURVE_Y), vec2(2.0));
    return c * 0.5 + 0.5;
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = curve(fragCoord / iResolution.xy);
    // past the edge of the tube: black
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        fragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    vec2 px = 1.0 / iResolution.xy;
    vec3 col;
    col.r = texture(iChannel0, uv + vec2(FRINGE, 0.0) * px).r;
    col.g = texture(iChannel0, uv).g;
    col.b = texture(iChannel0, uv - vec2(FRINGE, 0.0) * px).b;

    // scanlines: a soft dip between lines, following the curved glass
    float scan = 0.5 + 0.5 * cos(6.2831853 * uv.y * iResolution.y / SCANLINE_PERIOD);
    col *= 1.0 - SCANLINE_DEPTH * scan;

    // aperture grille: each column of three favors one channel
    int column = int(mod(fragCoord.x, 3.0));
    vec3 mask = vec3(1.0 - MASK_DEPTH);
    mask[column] = 1.0;
    col *= mask;

    // dark corners: x·y·(1-x)·(1-y) is 1/16 in the middle and 0 on the edges; the plateau
    // keeps most of the screen at full brightness
    vec2 edge = uv * (1.0 - uv.yx);
    col *= clamp(pow(edge.x * edge.y * 16.0, VIGNETTE) * VIGNETTE_PLATEAU, 0.0, 1.0);

    // give back the brightness the lines and the grille took
    col *= 1.0 + SCANLINE_DEPTH * 0.5 + MASK_DEPTH * 0.6;
    fragColor = vec4(clamp(col, 0.0, 1.0), 1.0);
}
