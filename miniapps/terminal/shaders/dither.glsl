// Dither: a pixel-art look. The screen in bigger pixels, drawn with four colors of the
// theme (background, a dim accent, the accent, the text) mixed by a 4×4 Bayer pattern.
// More for fun than for reading all day. Ghostty's custom-shader format (mainImage,
// iChannel0 = the terminal screen). Static (no clock).
//
// Each number #define is an adjustment in the Library; the [min, max] above it is its
// range.

// Pixel size
// [1, 8]
#define PIXEL 2.0
// How much of the effect
// [0, 1]
#define STRENGTH 1.0

// The 4×4 Bayer threshold of a cell, in 0..1: the 2×2 pattern (0 2 / 3 1) of the low
// bits, scaled, plus the one of the high bits.
float bayer(vec2 cell) {
    vec2 q = mod(cell, 4.0);
    vec2 lo = mod(q, 2.0);
    vec2 hi = floor(q / 2.0);
    float v = 4.0 * mod(2.0 * lo.x + 3.0 * lo.y, 4.0) + mod(2.0 * hi.x + 3.0 * hi.y, 4.0);
    return (v + 0.5) / 16.0;
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 cell = floor(fragCoord / PIXEL);
    vec4 c = texture(iChannel0, (cell + 0.5) * PIXEL / iResolution.xy);
    vec3 luma = vec3(0.299, 0.587, 0.114);
    float l = clamp(dot(c.rgb, luma), 0.0, 1.0);
    // the four colors go from the background to the text: on a light theme the background
    // is the light end, so the scale is turned around
    if (dot(iBackgroundColor, luma) > 0.5) {
        l = 1.0 - l;
    }
    l *= 3.0;
    float level = floor(l);
    float pick = level + step(bayer(cell), l - level);
    vec3 color = pick < 0.5 ? iBackgroundColor
        : pick < 1.5 ? mix(iBackgroundColor, iCursorColor, 0.45)
        : pick < 2.5 ? iCursorColor
        : iForegroundColor;
    fragColor = vec4(mix(c.rgb, color, STRENGTH), c.a);
}
