// Theme tint: pulls the screen toward the theme's colors, the dark parts toward the
// background and the light ones toward the cursor's color (the theme's accent). Each
// pixel keeps its brightness, so the text stays as readable. Ghostty's custom-shader
// format (mainImage, iChannel0 = the terminal screen). Static (no clock).
//
// Each number #define is an adjustment in the Library; the [min, max] above it is its
// range.

// How much of the theme's colors
// [0, 1]
#define STRENGTH 0.35
// Where the light color starts (brightness)
// [0, 1]
#define MIDPOINT 0.45

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec4 c = texture(iChannel0, fragCoord / iResolution.xy);
    vec3 luma = vec3(0.299, 0.587, 0.114);
    float l = dot(c.rgb, luma);
    vec3 duo = mix(iBackgroundColor, iCursorColor, smoothstep(MIDPOINT - 0.4, MIDPOINT + 0.4, l));
    // the same brightness as before
    duo = clamp(duo * (l / max(dot(duo, luma), 0.02)), 0.0, 1.0);
    fragColor = vec4(mix(c.rgb, duo, STRENGTH), c.a);
}
