// Focus dim: the screen gets darker and loses some color while the window is not in
// focus, with a short fade each way. Ghostty's custom-shader format (mainImage,
// iChannel0 = the terminal screen); it draws only around a focus change.
//
// Each number #define is an adjustment in the Library; the [min, max] above it is its
// range.
#pragma miniutils events

// How much darker without focus
// [0, 0.8]
#define DIM 0.25
// How much color goes without focus
// [0, 1]
#define DESATURATE 0.5
// Fade time (s)
// [0.05, 2]
#define FADE 0.25

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec4 c = texture(iChannel0, fragCoord / iResolution.xy);
    float k = clamp((iTime - iTimeFocus) / FADE, 0.0, 1.0);
    k = k * k * (3.0 - 2.0 * k);
    // 0 in focus, 1 away from it
    float away = iFocus == 1 ? 1.0 - k : k;
    float gray = dot(c.rgb, vec3(0.299, 0.587, 0.114));
    vec3 color = mix(c.rgb, vec3(gray), DESATURATE * away) * (1.0 - DIM * away);
    fragColor = vec4(color, c.a);
}
