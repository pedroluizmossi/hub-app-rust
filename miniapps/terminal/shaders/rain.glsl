// Rain: thin streaks fall over the background of the screen; the text stays clear.
// Ghostty's custom-shader format (mainImage, iChannel0 = the terminal screen). Animated:
// the window draws on every frame while it shows.
//
// The screen is cut in columns; each has a drop whose head falls and wraps, at its own
// speed, and rains on some passes only. Each number #define is an adjustment in the
// Library; the [min, max] above it is its range.

// Columns of rain across the screen
// [20, 200]
#define COLUMNS 70.0
// Fall speed (screens per second)
// [0.1, 3]
#define SPEED 0.9
// Streak length (part of the screen)
// [0.02, 0.4]
#define LENGTH 0.12
// How strongly it shows
// [0, 1]
#define OPACITY 0.35
// Only over the background (1) or over everything (0)
// [0, 1]
#define BEHIND 1.0

float hash(float n) {
    return fract(sin(n * 12.9898) * 43758.5453);
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;
    vec4 c = texture(iChannel0, uv);
    float column = floor(uv.x * COLUMNS);
    float speed = SPEED * (0.6 + 0.8 * hash(column));
    float fall = hash(column + 17.0) - iTime * speed;
    // the head goes down (GL's y grows upward); the streak is above it
    float head = fract(fall);
    float above = uv.y - head;
    if (above < 0.0) {
        above += 1.0;
    }
    float streak = above < LENGTH ? 1.0 - above / LENGTH : 0.0;
    // a thin line in the middle of the column
    float line = 1.0 - smoothstep(0.0, 0.12, abs(fract(uv.x * COLUMNS) - 0.5));
    // each pass of a column rains or not
    float raining = step(0.45, hash(column * 3.1 + floor(fall)));
    float rain = streak * streak * line * raining * OPACITY;
    // the background: on a dark theme the dark parts (a skin's backdrop included), on a
    // light one what is near the theme's background color
    vec3 luma = vec3(0.299, 0.587, 0.114);
    bool light = dot(iBackgroundColor, luma) > 0.5;
    float clear = light ? 1.0 - smoothstep(0.08, 0.25, length(c.rgb - iBackgroundColor))
                        : 1.0 - smoothstep(0.12, 0.35, dot(c.rgb, luma));
    float behind = mix(1.0, clear, BEHIND);
    vec3 color = mix(iBackgroundColor, iCursorColor, 0.7);
    fragColor = vec4(mix(c.rgb, color, rain * behind), c.a);
}
