// Pixel scenes: a pixel-art landscape behind the terminal's text, with scenes and weather
// that take turns, in the theme's colors. By Pedro Mossi, MIT. Ghostty's custom-shader
// format plus MiniUtils' iSeed (the tab's number) and iDate (the local time).
//
// Scenes:  0 city · 1 countryside · 2 mountains and lake · 3 coast · 4 avenue
// Weather: 0 rain · 1 clear sky · 2 fog
// Daytime: 0 morning · 1 afternoon · 2 night
//
// The text stays on top: every pixel whose color differs from the background is kept.
// The dark tones come from the theme's background, the light ones from its accent.
#pragma miniutils background

// -- SCENE, WEATHER AND DAYTIME (choices in the Library) ------------------------
// Scene: -2 a new one for each tab, -1 take turns (each tab starts on another), 0 to 4 one scene
#define SCENE -1.0
// Weather: -1 take turns, 0 to 2 one kind
#define WEATHER -1.0
// Daytime: -1 from the clock, 0 to 2 fixed
#define DAYTIME -1.0
// Minutes of each scene when they take turns
// [1, 60]
#define SCENE_MINUTES 10.0
// Minutes of each kind of weather when they take turns
// [1, 60]
#define WEATHER_MINUTES 7.0
const float SCENE_FADE   = 6.0;     // seconds of the change between scenes
const float WEATHER_FADE = 25.0;    // seconds of the change between kinds of weather

// Where the turns start and the daytime come from (MiniUtils' part, at the end).
float seedValue();   // where this tab's turns start
float timeOfDay();   // 0 morning, 1 afternoon, 2 night
#define SEED seedValue()
#define TIME_OF_DAY timeOfDay()

// -- LOOK ------------------------------------------------------------------------
// Size of an art pixel (screen pixels)
// [1, 8]
#define PIXEL 3.0
// Overall brightness (lower it if it gets in the way of reading)
// [0, 1]
#define BRIGHTNESS 0.85
// Haze toward the background color (0 = sharp)
// [0, 1]
#define HAZE 0.25
// On a light theme: the darkest tone of the scene (part of the paper's brightness)
// [0.3, 0.95]
#define LIGHT_SHADE 0.62
// On a light theme: how much of the art's own colors shows
// [0, 1]
#define LIGHT_COLOR 0.35
// Lit windows
// [0, 1]
#define WINDOWS 0.5
const bool  STARS          = false; // stars in the sky
const bool  ANTENNA_LIGHTS = true;  // red lights on the city's antennas
// Rain speed
// [0.1, 2]
#define RAIN_SPEED 0.55
const float RAIN_SLANT = -0.15;     // rain slant (0 = straight down; negative = to the left)
// Rain strength
// [0, 1]
#define RAIN_OPACITY 0.28
// Lightning during the rain (0 = none)
// [0, 0.6]
#define FLASH 0.18
// Fog density
// [0, 1.5]
#define FOG_AMOUNT 0.8
// Cars on the avenue
// [0, 1]
#define TRAFFIC 0.55
const float CAR_SPEED = 20.0;       // car speed (art pixels per second)
const float TEXT_CUT  = 0.06;       // least difference from the background that counts as text

// The light tones and the night's colors follow the theme's accent (the cursor's color).
#define TINT iCursorColor
const float TINT_AMOUNT = 0.20;
#define MOON mix(vec3(1.0), iCursorColor, 0.15)
const vec3 WINDOW_FAR  = vec3(0.520, 0.380, 0.300);
const vec3 WINDOW_NEAR = vec3(0.950, 0.720, 0.420);
#define RAIN_FAR (iCursorColor * 0.45)
#define RAIN_NEAR (iCursorColor * 0.75)
// ------------------------------------------------------------------------------

const float SCENE_COUNT = 5.0;


// Shared state (filled in by mainImage)
vec2  gRes;          // the art's resolution
float gT;          // time
float gCloudAmt;  // cloud amount (less in clear weather)
vec3  gTop, gHorizon, gCloud, gFar, gMid, gNear, gGround;
vec3  gSun;       // sun color (day) or moon color (night)
vec2  gSunPos;    // sun/moon position as a fraction of the screen
float gNight;     // 1 at night, 0 during the day
float gLights;    // fraction of artificial lights on
vec3  gW;         // weather weights: x = rain, y = clear, z = fog

float hash(float n) { return fract(sin(n * 127.1 + 311.7) * 43758.5453); }
float hash2(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
float tri(float u)  { return 1.0 - abs(fract(u) * 2.0 - 1.0); }

float bayer2(vec2 a) { a = floor(a); return fract(dot(a, vec2(0.5, a.y * 0.75))); }
float bayer4(vec2 a) { return bayer2(0.5 * a) * 0.25 + bayer2(a); }

vec3 ditherMix(vec3 a, vec3 b, float t, vec2 p, float bands) {
    float q = floor(clamp(t, 0.0, 1.0) * bands + bayer4(p)) / bands;
    return mix(a, b, clamp(q, 0.0, 1.0));
}

vec2 moonCenter() { return gRes * gSunPos; }

// ---- common parts: lights, vehicle colors, birds -----------------------
const vec3 HEADLIGHT = vec3(1.000, 0.940, 0.780);
const vec3 TAILLIGHT = vec3(0.950, 0.180, 0.220);
const vec3 AMBER     = vec3(1.000, 0.620, 0.150);
const vec3 FIREFLY   = vec3(0.850, 1.000, 0.450);
const vec3 AURORA    = vec3(0.300, 0.900, 0.700);

vec3 carPaint(float h) {
    if (h < 0.17) return vec3(0.55, 0.18, 0.20);   // red
    if (h < 0.34) return vec3(0.20, 0.30, 0.58);   // blue
    if (h < 0.50) return vec3(0.22, 0.46, 0.43);   // teal
    if (h < 0.66) return vec3(0.72, 0.72, 0.76);   // silver
    if (h < 0.82) return vec3(0.46, 0.26, 0.58);   // purple
    return vec3(0.30, 0.30, 0.34);                 // graphite
}

// 5 px bird flapping its wings (flap 1 = wings up in a V)
bool birdAt(vec2 p, vec2 c, float flap) {
    vec2 d = p - floor(c);
    float ax = abs(d.x);
    if (ax > 2.0) return false;
    if (flap > 0.5) return d.y == ax;
    return d.y == (ax == 2.0 ? -1.0 : 0.0);
}

// distance from p to segment a-b
float segDist(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a, ba = b - a;
    float h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-4), 0.0, 1.0);
    return length(pa - ba * h);
}

// type 0 = car (16 px), 1 = taxi (16 px), 2 = bus (26 px)
float carLength(float type) { return type > 1.5 ? 26.0 : 16.0; }

// side-view sprite facing +u; u and v start at 0 (wheel on the ground)
vec4 carSprite(float u, float v, float type, float h) {
    float L = carLength(type);
    if (u < 0.0 || u >= L || v < 0.0) return vec4(0.0);
    float lightK = mix(0.75, 0.42, gNight);
    vec3 tire  = gGround * 0.5;
    vec3 glass = mix(mix(gHorizon, gCloud, 0.5) * 0.9, mix(gMid, WINDOW_FAR, 0.3), gNight);
    vec3 tail  = TAILLIGHT * mix(0.6, 1.0, gNight);

    if (type > 1.5) {                                   // bus
        vec3 body = mix(gNear, vec3(0.25, 0.44, 0.32), lightK);
        if (v == 0.0) return (abs(u - 4.0) <= 1.0 || abs(u - 21.0) <= 1.0) ? vec4(tire, 1.0) : vec4(0.0);
        if (v > 7.0) return vec4(0.0);
        if (v == 7.0 && (u == 0.0 || u == L - 1.0)) return vec4(0.0);
        if (u == L - 1.0 && v == 2.0) return vec4(HEADLIGHT, 1.0);
        if (u == 0.0 && v == 2.0) return vec4(tail, 1.0);
        if (v == 6.0 && u >= 21.0 && u <= 24.0) return vec4(WINDOW_NEAR * mix(0.7, 1.0, gNight), 1.0); // sign
        if (v >= 4.0 && v <= 5.0 && u >= 2.0 && u <= 24.0 && mod(u - 2.0, 4.0) != 3.0) {
            // windows lit at night, each with a different shade
            float lit = hash(floor((u - 2.0) / 4.0) + h * 50.0);
            return vec4(mix(glass, WINDOW_NEAR * 0.8, gNight * step(0.35, lit) * 0.6), 1.0);
        }
        return vec4(body + (v == 7.0 ? 0.05 : 0.0), 1.0);
    }

    bool taxi = type > 0.5;
    vec3 body = mix(gNear, taxi ? vec3(0.88, 0.66, 0.18) : carPaint(h), lightK);
    if (v == 0.0) return (abs(u - 3.0) <= 1.0 || abs(u - 12.0) <= 1.0) ? vec4(tire, 1.0) : vec4(0.0);
    if (v <= 3.0) {
        if (v == 3.0 && (u == 0.0 || u == L - 1.0)) return vec4(0.0);
        if (u == L - 1.0 && v == 2.0) return vec4(HEADLIGHT, 1.0);
        if (u == 0.0 && v == 2.0) return vec4(tail, 1.0);
        if (v == 1.0 && (u == 0.0 || u == L - 1.0) && hash(h * 7.7) > 0.85 && fract(gT * 1.5) < 0.5) return vec4(AMBER, 1.0);   // hazard lights
        return vec4(body + (v == 3.0 ? 0.05 : 0.0), 1.0);
    }
    if (v == 4.0 && u >= 4.0 && u <= 11.0) return (u == 4.0 || u == 8.0 || u == 11.0) ? vec4(body, 1.0) : vec4(glass, 1.0);
    if (v == 5.0 && u >= 5.0 && u <= 10.0) return vec4(body, 1.0);
    if (taxi && v == 6.0 && (u == 7.0 || u == 8.0)) return vec4(WINDOW_NEAR, 1.0);
    return vec4(0.0);
}

// building of width w: random height between hmin and hmax; id = which building
float building(float x, float w, float seed, float hmin, float hmax, out float id) {
    id = floor(x / w);
    return floor(mix(hmin, hmax, hash(id + seed)));
}

// ---- shared sky: gradient, moon, stars, clouds -------------------------
vec3 sky(vec2 p) {
    float uy = p.y / gRes.y;
    vec3 col = ditherMix(gHorizon, gTop, pow(clamp(uy, 0.0, 1.0), 0.8), p, 6.0);

    vec2 mc = moonCenter();
    float r = max(7.0, floor(gRes.y * 0.045));
    float dm = length(p - mc);
    float halo = 1.0 - smoothstep(r, r * 2.6, dm);
    if (gNight > 0.5) {
        // crescent moon
        if (bayer4(p) < halo * 0.22) col = mix(col, gHorizon * 1.6, 0.5);
        if (dm < r && length(p - mc - vec2(r * 0.45, r * 0.25)) > r * 0.9) col = MOON;
    } else {
        // sun: full disc with a larger dithered glow
        float glow = 1.0 - smoothstep(r, r * 4.0, dm);
        if (bayer4(p) < glow * 0.35) col = mix(col, gSun, 0.22);
        if (dm < r * 1.1) col = gSun;
    }

    if (STARS && gNight > 0.5 && uy > 0.55) {
        float s = hash2(p);
        if (s > 0.9975) {
            float tw = 0.5 + 0.5 * sin(gT * (1.0 + 3.0 * hash(s * 91.0)) + s * 40.0);
            col = mix(col, vec3(0.75, 0.7, 0.95), 0.35 + 0.4 * tw);
        }
    }

    for (int i = 0; i < 2; i++) {
        float fi = float(i);
        float y0 = gRes.y * (0.70 + 0.12 * fi);
        float x = p.x + gT * (2.0 + 1.5 * fi);
        float top = y0 + 4.0 * sin(x * 0.045 + fi * 3.0) + 3.0 * sin(x * 0.11 + fi);
        float bot = y0 - 3.0 - 2.0 * sin(x * 0.07 + fi * 5.0);
        if (p.y < top && p.y > bot) {
            float edge = min(top - p.y, p.y - bot) / 3.0;
            if (bayer4(p) < clamp(edge, 0.0, 1.0) * 0.8 * gCloudAmt) col = gCloud + 0.03 * fi;
        }
    }
    return col;
}

// @scene city
// ---- city ------------------------------------------------------------
// airplane crossing the sky: silhouette by day, only flashing lights at night
vec3 airplane(vec2 p, vec3 col) {
    float span = gRes.x + 240.0;
    float run = gT * 9.0;
    float lap = floor(run / span);
    if (hash(lap + 77.0) < 0.35) return col;               // some laps without an airplane
    float x0 = floor(mod(run, span) - 120.0);
    float y0 = floor(gRes.y * (0.76 + 0.12 * hash(lap + 3.0)));
    vec2 d = p - vec2(x0, y0);
    bool body = d.y == 0.0 && d.x >= -6.0 && d.x <= 1.0;
    bool fin  = d.x == -6.0 && d.y >= 1.0 && d.y <= 2.0;
    bool wing = d.y == -1.0 && d.x >= -3.0 && d.x <= -2.0;
    if (body || fin || wing) col = mix(col, gNear * 0.8 + gFar * 0.2, mix(0.7, 0.25, gNight));
    if (d.x == -6.0 && d.y == 2.0 && fract(gT * 0.9) < 0.08) col = MOON;              // strobe
    if (d.x == -2.0 && d.y == 1.0 && fract(gT * 0.6 + 0.5) < 0.5) col = TAILLIGHT;    // red light
    return col;
}

// elevated train: viaduct with pillars and a 4-car train that passes occasionally
vec3 elevatedTrain(vec2 p, vec3 col, float ground) {
    float deck = ground + floor(gRes.y * 0.13);
    vec3 steel = mix(gFar, gNear, 0.55);
    if (p.y < deck && mod(p.x, 28.0) < 2.0) col = steel;                         // pillars
    if (p.y >= deck && p.y < deck + 2.0) col = steel;                             // viaduct
    if (mod(p.x, 28.0) == 0.0 && p.y >= deck + 2.0 && p.y <= deck + 11.0) col = steel;   // wire poles
    if (p.y == deck + 11.0 && bayer4(p) < 0.6) col = mix(col, steel, 0.8);       // wire

    const float CAR = 28.0, N = 4.0, SPEED = 45.0;
    float len = CAR * N;
    float cycle = gRes.x + len + 1200.0;
    float pos = mod(gT * SPEED, cycle);                     // front of the train
    float tx = p.x - (pos - len);
    float v = p.y - (deck + 2.0);
    if (tx < 0.0 || tx >= len || v < 0.0 || v > 10.0) return col;
    float ci = floor(tx / CAR), u = mod(tx, CAR);
    bool first = ci == N - 1.0, last = ci == 0.0;
    vec3 body = mix(gFar, vec3(0.70, 0.72, 0.78), mix(0.55, 0.30, gNight));
    vec3 glass = mix(mix(gHorizon, gCloud, 0.5), mix(gMid, WINDOW_FAR, 0.3), gNight);

    // pantograph and sparks on the wire (at night)
    if ((ci == 1.0 || ci == 2.0) && v >= 7.0 && v <= 8.0 && abs(u - 12.0 - (v - 7.0)) < 0.5) col = steel;
    if ((ci == 1.0 || ci == 2.0) && gNight > 0.5 && v >= 8.0 && v <= 10.0 && abs(u - 13.0) <= 1.0
        && hash(floor(gT * 12.0) + ci * 3.0) > 0.86 && bayer4(p) < 0.7) col = mix(MOON, TINT, 0.3);
    if (v > 6.0) return col;

    if (u >= CAR - 1.0) return col;                                              // gap between cars
    if (v == 0.0) return (abs(u - 5.0) <= 2.0 || abs(u - 21.0) <= 2.0) ? gNear * 0.6 : col;   // bogies
    if (v == 6.0 && (u < 1.0 || u > CAR - 3.0)) return col;                      // roof corners
    if (first && u == CAR - 2.0 && v == 2.0) return HEADLIGHT;
    if (last && u == 0.0 && v == 2.0) return TAILLIGHT * mix(0.6, 1.0, gNight);
    if (v >= 3.0 && v <= 4.0 && u >= 2.0 && u <= 24.0 && mod(u - 2.0, 5.0) < 3.0) {
        float lit = hash(floor((u - 2.0) / 5.0) + ci * 7.0 + floor(pos / cycle));
        return mix(glass, WINDOW_NEAR * (0.75 + 0.25 * lit), gNight * 0.85);
    }
    if (v == 2.0) return mix(body, TINT, 0.35);                                  // purple stripe
    return body;
}

// water tanks and billboards on the front buildings' rooftops
vec3 rooftop(vec2 p, vec3 col, float hN, float idN) {
    float u = mod(p.x, 15.0), y = p.y - hN;
    float kind = hash(idN + 11.0);
    if (kind > 0.55 && kind < 0.8) {                                             // water tank
        if ((u == 4.0 || u == 8.0) && y >= 0.0 && y < 2.0) col = gNear;
        if (u >= 4.0 && u <= 8.0 && y >= 2.0 && y <= 5.0) col = gNear;
        if (u >= 5.0 && u <= 7.0 && y == 6.0) col = gNear;
    } else if (kind >= 0.8) {                                                    // illuminated billboard
        if ((u == 3.0 || u == 11.0) && y >= 0.0 && y < 2.0) col = gNear;
        if (u >= 2.0 && u <= 12.0 && y >= 2.0 && y <= 7.0) {
            bool edge = u == 2.0 || u == 12.0 || y == 2.0 || y == 7.0;
            vec3 face = mix(gNear, carPaint(hash(idN + 12.0)), mix(0.45, 0.85, gNight));
            if ((y == 4.0 || y == 5.0) && u > 3.0 && u < 11.0 && hash(floor(u / 2.0) + idN * 3.0) > 0.45) face *= 0.55;
            col = edge ? gNear : face;
        }
    }
    return col;
}

// pedestrians with umbrellas on the sidewalk (only in rain)
vec3 walkers(vec2 p, vec3 col, float feet, float dir, float speed, float lane) {
    const float S = 26.0;
    float x = p.x - dir * gT * speed;
    float slot = floor(x / S);
    if (hash(slot * 2.3 + lane * 13.0) > 0.45) return col;
    float off = 4.0 + floor(hash(slot + lane * 5.0) * (S - 10.0));
    float u = floor(mod(x, S)) - off;
    float v = p.y - feet;
    if (u < -2.0 || u > 3.0 || v < 0.0 || v > 8.0) return col;
    vec3 body = gNear * 0.8 + gMid * 0.2;
    float stepF = step(0.5, fract(gT * 2.0 + hash(slot)));
    if (v <= 1.0 && (u == 0.0 || u == 1.0) && !(v == 0.0 && u == stepF)) col = body;     // legs
    if (v >= 2.0 && v <= 5.0 && (u == 0.0 || u == 1.0)) col = body;                     // body and head
    if (v == 6.0 && u == 1.0) col = body;                                                // handle
    vec3 umb = mix(gNear, carPaint(hash(slot * 1.3 + lane)), 0.65);
    if ((v == 7.0 && u >= -2.0 && u <= 3.0) || (v == 8.0 && u >= -1.0 && u <= 2.0)) col = umb;
    return col;
}

vec3 sceneCity(vec2 p) {
    float wRain = gW.x;
    vec3 col = airplane(p, sky(p));
    float ground = floor(gRes.y * 0.06);

    float idF;
    float hF = ground + building(p.x + 7.0, 9.0, 1.0, gRes.y * 0.18, gRes.y * 0.36, idF);
    if (p.y < hF) {
        col = gFar;
        vec2 w = vec2(mod(p.x + 7.0, 9.0), p.y - ground);
        if (w.x > 1.0 && w.x < 8.0 && mod(w.x, 3.0) == 2.0 && mod(w.y, 4.0) == 2.0 && p.y < hF - 2.0) {
            float lit = hash2(vec2(idF, floor(w.y / 4.0)) + floor(w.x / 3.0) * 7.0);
            if (lit > 1.0 - 0.28 * WINDOWS * gLights) col = WINDOW_FAR;
        }
    }

    col = elevatedTrain(p, col, ground);

    float idN;
    float hN = ground + building(p.x, 15.0, 7.0, gRes.y * 0.08, gRes.y * 0.24, idN);
    float antenna = step(0.75, hash(idN + 3.0)) * step(abs(mod(p.x, 15.0) - 7.0), 0.0);
    if (antenna == 0.0 && step(0.75, hash(idN + 3.0)) == 0.0 && p.y >= hN) col = rooftop(p, col, hN, idN);
    if (p.y < hN || (antenna > 0.0 && p.y < hN + 5.0)) {
        col = gNear;
        if (ANTENNA_LIGHTS && antenna > 0.0 && p.y == hN + 4.0 && fract(gT * 0.5 + hash(idN)) < 0.5)
            col = vec3(0.9, 0.2, 0.3);
        vec2 w = vec2(mod(p.x, 15.0), p.y - ground);
        if (p.y < hN - 2.0 && w.x > 1.0 && w.x < 14.0 && mod(w.x, 4.0) != 1.0 && mod(w.y, 5.0) > 1.0 && mod(w.y, 5.0) < 4.0) {
            vec2 cell = vec2(idN * 10.0 + floor(w.x / 4.0), floor(w.y / 5.0));
            float lit = hash2(cell);
            float flick = step(0.5, fract(gT * 0.02 + hash2(cell + 5.0)));
            float wl = WINDOWS * gLights;
            if (lit > 1.0 - 0.22 * wl || (lit > 1.0 - 0.30 * wl && flick > 0.0)) col = WINDOW_NEAR;
        }
    }

    if (p.y < ground) {
        col = gGround;
        float refl = hash2(vec2(floor(p.x / 2.0), floor(gT * 3.0)));
        if (bayer4(p) < 0.12 && refl > 0.7) col = mix(gGround, WINDOW_NEAR, 0.18);
        // splashes and umbrellas (only in rain)
        if (wRain > 0.3) {
            float cell = floor(p.x / 5.0);
            float ph = fract(gT * (0.9 + 0.6 * hash(cell)) + hash(cell + 9.0));
            float sx = mod(p.x, 5.0);
            float sy = ground - 1.0 - p.y;
            if (ph < 0.12 && sy < 1.0 && (sx == 1.0 || sx == 3.0)) col = RAIN_FAR;
            if (ph < 0.06 && sy < 1.0 && sx == 2.0) col = RAIN_NEAR;
        }
    }
    if (wRain > 0.3 && p.y < ground + 6.0) {
        float feet = floor(ground * 0.35);
        col = walkers(p, col, feet + 3.0, -1.0, 5.0, 1.0);
        col = walkers(p, col, feet, 1.0, 6.0, 0.0);
    }
    return col;
}

// @scene countryside
// ---- countryside with hills ---------------------------------------------
float hillFar(float x)  { return floor(gRes.y * 0.26 + 6.0 * sin(x * 0.020 + 1.0) + 4.0 * sin(x * 0.051 + 2.0)); }
float hillMid(float x)  { return floor(gRes.y * 0.18 + 5.0 * sin(x * 0.030 + 4.0) + 3.0 * sin(x * 0.077)); }
float hillNear(float x) { return floor(gRes.y * 0.10 + 4.0 * sin(x * 0.025 + 0.5) + 2.0 * sin(x * 0.090 + 3.0)); }

// flock of birds crossing the sky in a V during the day
vec3 birdFlock(vec2 p, vec3 col, float yFrac) {
    if (gNight > 0.5) return col;
    float span = gRes.x + 300.0;
    float run = gT * 9.0;
    float lap = floor(run / span);
    if (hash(lap + 41.0) < 0.3) return col;
    vec2 c = vec2(mod(run, span) - 150.0, floor(gRes.y * (yFrac + 0.08 * hash(lap + 2.0))));
    if (abs(p.x - c.x) > 20.0 || abs(p.y - c.y) > 12.0) return col;
    vec3 ink = mix(gTop, gNear, 0.75);
    for (int i = -2; i <= 2; i++) {
        float fi = float(i);
        vec2 b = c + vec2(-abs(fi) * 5.0, fi * 3.0 + floor(sin(gT * 0.8 + fi) + 0.5));
        if (birdAt(p, b, step(0.5, fract(gT * 2.5 + fi * 0.37)))) col = ink;
    }
    return col;
}

// windmill on the far hill, with rotating blades
vec3 windmill(vec2 p, vec3 col) {
    float mx = floor(gRes.x * 0.72);
    float base = hillFar(mx) - 2.0;
    vec3 ink = mix(gFar, gNear, 0.55);
    float dx = p.x - mx, dy = p.y - base;
    if (dy >= 0.0 && dy < 18.0 && abs(dx) <= floor(2.5 - dy * 0.09)) {
        col = ink;
        if (dx == 0.0 && dy == 9.0 && WINDOWS * gLights > 0.1) col = WINDOW_NEAR;
    }
    vec2 hub = vec2(mx, base + 18.0);
    vec2 d = p - hub;
    if (length(d) < 13.0) {
        for (int k = 0; k < 4; k++) {
            float a = gT * 0.9 + float(k) * 1.5708;
            vec2 dirv = vec2(cos(a), sin(a));
            float along = dot(d, dirv);
            float across = abs(d.x * dirv.y - d.y * dirv.x);
            if (along > 0.5 && along < 12.0 && across < 0.6 + along * 0.06) col = ink;
        }
        if (length(d) < 1.0) col = ink;
    }
    return col;
}

vec3 sceneHills(vec2 p) {
    vec3 col = birdFlock(p, sky(p), 0.60);
    if (p.y < hillFar(p.x)) col = gFar;
    col = windmill(p, col);
    if (p.y < hillMid(p.x)) col = gMid;

    // dirt road on the middle hill, with an occasional car
    float roadX0 = floor(gRes.x * 0.36);
    if (p.x >= roadX0) {
        float rm = hillMid(p.x);
        if (p.y >= rm - 5.0 && p.y <= rm - 3.0 && bayer4(p) < 0.7) col = mix(gMid, gHorizon, 0.30);
        float span = gRes.x - roadX0 + 400.0;
        float cx = roadX0 - 20.0 + mod(gT * 10.0, span);
        if (cx < gRes.x + 20.0) {
            float base = hillMid(floor(cx + 8.0)) - 4.0;
            float u = p.x - floor(cx), v = p.y - base;
            float fogK = gW.z;
            float du = u - 16.0;
            if (du >= 0.0 && du < 18.0 + 8.0 * fogK && abs(v - 1.0) <= 1.0 + du * (0.08 + 0.12 * fogK)) {
                float inten = (1.0 - du / (18.0 + 8.0 * fogK)) * 0.55 * max(gNight, fogK * 0.5);
                if (bayer4(p) < inten) col = mix(col, HEADLIGHT, 0.22);
            }
            vec4 c = carSprite(u, v, 0.0, hash(floor(gT * 10.0 / span) + 5.0));
            col = mix(col, c.rgb, c.a);
        }
    }

    // small house on the middle hill, with a lit window and chimney
    float hx = floor(gRes.x * 0.28);
    float hb = hillMid(hx) - 1.0;
    float dx = p.x - hx, dy = p.y - hb;
    bool body = abs(dx) <= 5.0 && dy >= 0.0 && dy < 7.0;
    bool roof = dy >= 7.0 && abs(dx) <= 13.0 - dy;
    bool chim = dx >= 2.0 && dx <= 3.0 && dy >= 7.0 && dy < 12.0;
    if (body || roof || chim) {
        col = gNear;
        if (dx >= -3.0 && dx <= -2.0 && dy >= 2.0 && dy <= 3.0 && WINDOWS * gLights > 0.1) col = WINDOW_NEAR;
    }

    // chimney smoke rising and dispersing in the wind
    vec2 top = vec2(hx + 2.5, hb + 12.0);
    if (p.y >= top.y && p.y < top.y + 34.0 && p.x > top.x - 6.0 && p.x < top.x + 24.0) {
        for (int k = 0; k < 6; k++) {
            float ph = fract(gT * 0.12 + float(k) / 6.0);
            vec2 c = top + vec2(ph * ph * 16.0 + sin(ph * 6.0 + float(k)) * 1.5, ph * 30.0);
            float r = (1.0 + ph * 3.0) * (1.0 + 0.6 * gW.z);
            if (length(p - c) < r && bayer4(p) < (1.0 - ph) * (0.65 - 0.25 * gW.z)) col = mix(col, gCloud * 1.3, 0.65);
        }
    }

    float hn = hillNear(p.x);
    if (p.y < hn) col = gGround;

    // fence on the front hill
    if (p.x < gRes.x * 0.45) {
        float yb = p.y - hn;
        if ((mod(p.x, 6.0) == 0.0 && yb >= 0.0 && yb < 4.0) || (yb == 2.0)) col = gGround;
    }

    // trees on the front hill: round trees and pines
    float id = floor(p.x / 13.0);
    for (int k = -1; k <= 1; k++) {
        float i = id + float(k);
        float h = hash(i + 21.0);
        if (h < 0.5) continue;
        float cx = i * 13.0 + 6.0;
        float base = hillNear(cx);
        float ddx = p.x - cx, ddy = p.y - base;
        if (h > 0.78) {
            float th = 9.0 + floor(hash(i + 3.0) * 8.0);        // pine
            if (ddy >= 0.0 && ddy < th && abs(ddx) <= floor((th - ddy) * 0.4)) col = gGround;
        } else {
            float r = 3.0 + floor(hash(i + 5.0) * 2.0);          // round tree
            if ((ddx == 0.0 && ddy >= 0.0 && ddy < 3.0) || length(vec2(ddx, ddy - 3.0 - r)) <= r) col = gGround;
        }
    }

    // fireflies over the grass (clear night)
    float ff = gNight * gW.y;
    if (ff > 0.05 && p.y > hn - 8.0 && p.y < hn + 16.0) {
        vec2 cs = vec2(11.0, 8.0);
        vec2 cell = floor(p / cs);
        float h = hash2(cell + 31.0);
        if (h > 0.55) {
            vec2 c = cell * cs + vec2(3.0 + 5.0 * hash2(cell + 2.0), 2.0 + 4.0 * hash2(cell + 4.0))
                   + vec2(sin(gT * 0.7 + h * 20.0), cos(gT * 0.5 + h * 13.0)) * 1.5;
            float blink = pow(0.5 + 0.5 * sin(gT * 2.0 + h * 30.0), 6.0);
            if (all(equal(p, floor(c + 0.5)))) col = mix(col, FIREFLY, blink * ff);
        }
    }
    return col;
}

// @scene mountains and lake
// ---- mountains and lake ----------------------------------------------
float lakeLevel() { return floor(gRes.y * 0.22); }

// occasional shooting star (clear night)
vec3 shootingStar(vec2 p, vec3 col) {
    float k = gNight * gW.y;
    if (k < 0.05) return col;
    float win = floor(gT / 7.0);
    if (hash(win + 11.0) < 0.55) return col;
    float ph = fract(gT / 7.0) * 7.0 / 0.9;
    if (ph > 1.0) return col;
    vec2 s0 = vec2(gRes.x * (0.2 + 0.6 * hash(win)), gRes.y * (0.72 + 0.2 * hash(win + 1.0)));
    vec2 dirv = normalize(vec2(hash(win + 2.0) > 0.5 ? 1.0 : -1.0, -0.4));
    vec2 head = s0 + dirv * ph * 50.0;
    vec2 d = p - head;
    float t = dot(d, -dirv);
    if (t >= 0.0 && t < 12.0 && abs(d.x * dirv.y - d.y * dirv.x) < 0.6)
        col = mix(col, MOON, (1.0 - t / 12.0) * (1.0 - ph * 0.6) * k);
    return col;
}

// aurora: bands waving behind the peaks, rare and only at night
vec3 aurora(vec2 p, vec3 col) {
    float a = gNight * gW.y * smoothstep(0.55, 0.85, 0.5 + 0.5 * sin(gT * 0.05 + SEED));
    if (a < 0.02) return col;
    float y0 = gRes.y * 0.60 + 8.0 * sin(p.x * 0.02 + gT * 0.3) + 4.0 * sin(p.x * 0.057 - gT * 0.2);
    float d = p.y - y0;
    if (d < 0.0 || d > 30.0) return col;
    float rays = pow(0.5 + 0.5 * sin(p.x * 0.3 + gT * 1.5 + sin(p.x * 0.05) * 3.0), 2.0);
    float inten = (1.0 - d / 30.0) * (0.35 + 0.65 * rays) * a;
    if (bayer4(p) < inten * 0.42) col = mix(col, mix(AURORA, TINT, d / 30.0), 0.32);
    return col;
}

// cabin on the shore with a campfire (the fire goes out in the rain)
vec3 cabin(vec2 p, vec3 col, float lake) {
    float cx = floor(gRes.x * 0.62);
    float base = lake + 3.0;
    float dx = p.x - cx, dy = p.y - base;
    if ((abs(dx) <= 4.0 && dy >= 0.0 && dy <= 4.0) || (dy >= 5.0 && dy <= 8.0 && abs(dx) <= 10.0 - dy)) {
        col = gNear;
        if (dx >= -2.0 && dx <= -1.0 && dy >= 1.0 && dy <= 2.0 && WINDOWS * gLights > 0.1) col = WINDOW_NEAR;
    }
    if (gW.x > 0.5) return col;
    float fx = p.x - (cx + 9.0);
    float glowD = length(vec2(fx, dy - 1.0));
    if (glowD < 7.0 && bayer4(p) < (1.0 - glowD / 7.0) * 0.3 * gNight) col = mix(col, AMBER, 0.25);
    if (dy == 0.0 && abs(fx) <= 2.0) col = gNear * 0.8 + AMBER * 0.1;        // firewood
    float fh = 2.0 + floor(3.0 * hash(floor(gT * 8.0) + fx * 3.1));
    if (abs(fx) <= 1.0 && dy >= 1.0 && dy < fh + 1.0 - abs(fx))
        col = mix(AMBER, TAILLIGHT, (dy - 1.0) / fh * 0.6);
    return col;
}

vec3 lakeAbove(vec2 p) {
    vec3 col = aurora(p, shootingStar(p, sky(p)));
    float lake = lakeLevel();
    float m = gRes.y * (0.15 * tri(p.x / 90.0 + 0.3) + 0.07 * tri(p.x / 37.0 + 0.7) + 0.02 * tri(p.x / 13.0));
    float hFar = lake + floor(gRes.y * 0.06 + m);
    if (p.y < hFar) {
        col = mix(gFar, gNear, 0.45);
        // snow on the peaks
        if (p.y > lake + gRes.y * 0.21 && p.y > hFar - gRes.y * 0.05 && bayer4(p) < 0.55) col = mix(gFar, MOON, 0.18);
    }
    float hMid = lake + floor(gRes.y * (0.03 + 0.06 * tri(p.x / 55.0 + 0.1) + 0.025 * tri(p.x / 19.0 + 0.4)));
    if (p.y < hMid) col = gMid;
    // pines on the shore
    float id = floor(p.x / 5.0);
    for (int k = -1; k <= 1; k++) {
        float i = id + float(k);
        float cx = i * 5.0 + 2.0;
        float top = lake + 4.0 + floor(hash(i + 13.0) * 9.0);
        if (p.y < top && abs(p.x - cx) <= floor((top - p.y) * 0.45)) col = gNear;
    }
    if (p.y < lake + 3.0) col = gNear;
    col = cabin(p, col, lake);
    return col;
}

// rowboat crossing the lake, with a lantern at night
vec3 rowboat(vec2 p, vec3 col, float lake) {
    float base = floor(lake * 0.5);
    float bx = floor(mod(gT * 4.0, gRes.x + 80.0) - 40.0);
    float u = p.x - bx, v = p.y - base;
    if (u < -12.0 || u > 13.0 || v < -8.0 || v > 5.0) return col;
    if (v == 0.0 && u >= -10.0 && u <= -1.0 && bayer4(p) < (1.0 + u / 11.0) * 0.4) col = mix(col, gHorizon * 1.3, 0.4);   // wake
    if (u == 10.0 && v >= -7.0 && v <= -1.0 && bayer4(p) < (1.0 + v / 8.0) * 0.6 * gNight) col = mix(col, WINDOW_NEAR, 0.35);   // lantern reflection
    float oar = step(0.5, fract(gT * 0.8));
    if (segDist(p, vec2(bx + 5.0, base + 2.0), vec2(bx + mix(2.0, 8.0, oar), base - 1.0)) < 0.6) col = gNear;
    if ((v == 0.0 && u >= 1.0 && u <= 10.0) || (v == 1.0 && u >= 0.0 && u <= 11.0)) col = v == 1.0 ? mix(gNear, gHorizon, 0.15) : gNear;
    if (u == 6.0 && v >= 2.0 && v <= 4.0) col = gNear * 0.9 + gMid * 0.1;          // rower
    if (u == 10.0 && v == 2.0) col = gNear;
    if (u == 10.0 && v == 3.0) col = mix(gNear, WINDOW_NEAR, max(gNight, 0.2));      // lantern
    return col;
}

vec3 sceneLake(vec2 p) {
    float lake = lakeLevel();
    if (p.y >= lake) return lakeAbove(p);
    // rippling reflection on the water
    float dx = floor(1.5 * sin(p.y * 0.9 + gT * 1.2));
    vec3 r = lakeAbove(vec2(p.x + dx, 2.0 * lake - p.y - 1.0));
    vec3 col = mix(gGround, r, 0.55);
    if (bayer4(p) < 0.12 && sin(p.y * 1.3 + gT * 0.8 + p.x * 0.02) > 0.8) col = mix(col, gHorizon * 1.3, 0.5);
    // raindrops making circles on the water
    if (gW.x > 0.1) {
        vec2 cs = vec2(14.0, 6.0);
        vec2 cell = floor(p / cs);
        float h = hash2(cell + 7.0);
        if (h > 0.45) {
            float ph = fract(gT * 0.7 + h * 3.0);
            vec2 c = cell * cs + vec2(4.0 + 6.0 * hash2(cell + 1.0), 3.0);
            if (abs(length((p - c) * vec2(1.0, 2.2)) - ph * 5.0) < 0.6) col = mix(col, gHorizon * 1.3, (1.0 - ph) * 0.5 * gW.x);
        }
    }
    col = rowboat(p, col, lake);
    if (p.y < floor(gRes.y * 0.04)) col = gGround;
    return col;
}

// @scene coast
// ---- coast with lighthouse ---------------------------------------------
float cliffTop(float x) {
    float x0 = gRes.x * 0.70;
    return floor(gRes.y * 0.24 + min((x - x0) * 0.9, gRes.y * 0.12) + 2.0 * sin(x * 0.3));
}
float rockTop(float x) {
    return floor(gRes.y * 0.05 + 4.0 * max(0.0, sin(x * 0.09 + 1.0)) + 2.0 * sin(x * 0.31));
}

// cargo ship moving slowly on the horizon, with navigation lights
vec3 cargoShip(vec2 p, vec3 col, float sea) {
    float sx = floor(gRes.x * 1.05 - mod(gT * 1.2, gRes.x * 1.3));
    float u = p.x - sx, v = p.y - sea;
    if (u < -1.0 || u > 26.0 || v < 0.0 || v > 7.0) return col;
    vec3 ink = mix(gFar, gNear, 0.5);
    if ((v == 0.0 && u >= 1.0 && u <= 24.0) || (v == 1.0 && u >= 0.0 && u <= 25.0)) col = ink;      // hull
    if (v >= 2.0 && v <= 3.0 && u >= 3.0 && u <= 15.0 && mod(u - 3.0, 3.0) < 2.0)                  // containers
        col = mix(ink, carPaint(hash(floor((u - 3.0) / 3.0) + v * 5.0)), mix(0.45, 0.2, gNight));
    if (v >= 2.0 && v <= 4.0 && u >= 18.0 && u <= 23.0) {                                           // bridge
        col = ink;
        if (v == 3.0 && mod(u, 2.0) == 1.0) col = mix(ink, WINDOW_NEAR, max(gNight, 0.15));
    }
    if (v >= 5.0 && v <= 6.0 && u >= 20.0 && u <= 21.0) col = ink;                                   // funnel
    if (u == 12.0 && v >= 2.0 && v <= 6.0) col = ink;                                                // mast
    if (u == 12.0 && v == 7.0) col = mix(ink, MOON, max(gNight, 0.2));
    if (u == 0.0 && v == 2.0) col = mix(ink, TAILLIGHT, max(gNight, 0.2));
    if (u == 25.0 && v == 2.0) col = mix(ink, vec3(0.3, 0.9, 0.4), max(gNight, 0.2));
    return col;
}

// sailboat rocking on the waves
vec3 sailboat(vec2 p, vec3 col, float sea) {
    float base = floor(sea * 0.55) + floor(sin(gT * 1.4) * 0.9 + 0.5);
    float bx = floor(mod(gT * 3.0, gRes.x * 0.75 + 40.0) - 20.0);
    float u = p.x - bx, v = p.y - base;
    if (u < -1.0 || u > 13.0 || v < 0.0 || v > 15.0) return col;
    vec3 sail = mix(gHorizon, MOON, 0.35) * (1.0 - 0.3 * gNight);
    float w = floor((14.0 - v) * 0.42);
    if (v >= 3.0 && v <= 13.0 && u >= 6.0 - w && u <= 5.0) col = sail;                              // mainsail
    if (v >= 4.0 && v <= 11.0 && u >= 7.0 && u <= 7.0 + floor((12.0 - v) * 0.3)) col = sail * 0.9;  // jib
    if (u == 6.0 && v >= 2.0 && v <= 14.0) col = gNear;                                              // mast
    if ((v == 0.0 && u >= 1.0 && u <= 10.0) || (v == 1.0 && u >= 0.0 && u <= 11.0)) col = gNear;    // hull
    if (u == 6.0 && v == 15.0) col = mix(gNear, WINDOW_NEAR, max(gNight, 0.1));                       // masthead light
    return col;
}

vec3 sceneCoast(vec2 p) {
    vec3 col = sky(p);
    float sea = floor(gRes.y * 0.24);
    vec2 mc = moonCenter();

    col = cargoShip(p, col, sea);

    // seagulls gliding near the cliff (during the day)
    if (gNight < 0.5) {
        vec2 gc = vec2(gRes.x * 0.62, gRes.y * 0.46);
        for (int i = 0; i < 3; i++) {
            float fi = float(i);
            vec2 b = gc + vec2(cos(gT * 0.35 + fi * 2.1) * 22.0, sin(gT * 0.5 + fi * 1.7) * 6.0 + fi * 4.0);
            if (birdAt(p, b, step(0.5, fract(gT * 2.2 + fi * 0.37)))) col = mix(MOON, gHorizon, 0.35);
        }
    }

    if (p.y < sea) {
        float depth = 1.0 - p.y / sea;                        // 0 at the horizon, 1 near
        col = ditherMix(gFar, gGround, depth, p, 4.0);
        float wave = sin(p.x * (0.12 - 0.08 * depth) + gT * (0.6 + 0.8 * depth) + hash(p.y) * 6.28);
        if (wave > 0.93) col = mix(col, gHorizon * 1.4, 0.6);
        // path of moonlight on the sea
        float w = 2.0 + depth * 10.0;
        if (abs(p.x - mc.x) < w && hash2(vec2(floor(p.x / 2.0), p.y + floor(gT * 2.0))) > 0.55)
            col = mix(col, gSun, 0.35 * (1.0 - depth * 0.5));
        // foam and sand on the shore
        float shore = floor(gRes.y * 0.05 + 1.5 + 1.5 * sin(p.x * 0.05 + gT * 0.7));
        if (p.y < shore) col = gGround * 1.2;
        if (abs(p.y - shore) < 1.0 && bayer4(p) < 0.6) col = mix(gHorizon, MOON, 0.3);
    }

    col = sailboat(p, col, sea);

    // rotating lighthouse beam (a visible cone in rain and fog)
    float lx = floor(gRes.x * 0.86);
    float base = cliffTop(lx);
    float th = floor(max(20.0, gRes.y * 0.12));   // tower height
    vec2 lamp = vec2(lx, base + th + 1.0);
    float ang = gT * 0.7;
    float dirx = cos(ang);
    float len = abs(dirx) * gRes.x * 0.5;
    float bdx = p.x - lx;
    float wk = clamp(gW.x + gW.z, 0.0, 1.0);
    if (bdx * dirx > 0.0 && abs(bdx) < len && abs(p.y - lamp.y) < 1.0 + abs(bdx) * (0.08 + 0.1 * wk)) {
        float inten = (1.0 - abs(bdx) / len) * 0.45 * max(gNight, wk * 0.5) * (1.0 + wk);
        if (bayer4(p) < inten) col = mix(col, WINDOW_NEAR, 0.35);
    }

    // cliff
    if (p.x > gRes.x * 0.70 && p.y < cliffTop(p.x)) col = gNear;

    // lighthouse keeper's house at the foot of the tower
    float hx = lx + 9.0;
    float hdx = p.x - hx, hdy = p.y - cliffTop(hx);
    if ((abs(hdx) <= 4.0 && hdy >= 0.0 && hdy <= 4.0) || (hdy >= 5.0 && hdy <= 8.0 && abs(hdx) <= 10.0 - hdy)) {
        col = gNear;
        if (hdx >= -2.0 && hdx <= -1.0 && hdy >= 1.0 && hdy <= 2.0 && WINDOWS * gLights > 0.1) col = WINDOW_NEAR;
    }

    // striped lighthouse, lantern room and roof
    float dy = p.y - base;
    float hw = floor(4.0 - 2.0 * dy / th + 0.5);
    float stripe = floor(th / 5.0);
    if (dy >= 0.0 && dy < th && abs(p.x - lx) <= hw)
        col = mod(floor(dy / stripe), 2.0) == 0.0 ? mix(gHorizon, MOON, 0.30) : gNear;
    if (dy >= th && dy < th + 3.0 && abs(p.x - lx) <= 1.0)
        col = mix(gMid, WINDOW_NEAR, (0.5 + 0.5 * abs(dirx)) * max(gNight, 0.2));
    if (dy >= th + 3.0 && dy < th + 5.0 && abs(p.x - lx) <= th + 5.0 - dy) col = gNear;

    // foreground rocks, with waves breaking into foam
    if (p.x < gRes.x * 0.25) {
        float rh = rockTop(p.x);
        if (p.y < rh) col = gNear * 0.8;
        float cell = floor(p.x / 12.0);
        float ph = fract(gT * 0.22 + hash(cell + 40.0));
        float hgt = ph < 0.4 ? sin(ph / 0.4 * 3.1416) * 9.0 : 0.0;
        float sy = p.y - rh;
        if (hgt > 0.5 && sy >= 0.0 && sy < hgt && hash2(vec2(p.x, p.y + floor(gT * 12.0))) > 0.45 + sy / hgt * 0.4)
            col = mix(gHorizon, MOON, 0.45);
    }
    return col;
}

// @scene avenue
// ---- avenue with cars ----------------------------------------------
// one direction of the avenue: cars in slots moving together (they never collide)
// base = line where wheels touch; dir = +1 right, -1 left
vec3 laneTraffic(vec2 p, vec3 col, float base, float dir, float speed, float lane, float wRain) {
    const float S = 72.0;                                  // size of each slot
    float x = p.x - dir * gT * speed;
    float slot = floor(x / S);
    float xs = floor(mod(x, S));
    float u = dir > 0.0 ? xs : S - 1.0 - xs;               // coordinate with the car facing +u
    float hc = hash(slot * 1.7 + lane * 31.0);
    if (hc > TRAFFIC) return col;
    float ht = hash(slot * 3.1 + lane * 17.0);
    float type = ht < 0.12 ? 2.0 : (ht < 0.27 ? 1.0 : 0.0);
    float L = carLength(type);
    float off = floor(hash(slot + lane * 7.0 + 0.5) * (S - L - 22.0));
    float cu = u - off;
    float v = p.y - base;

    // headlight beams on the road (night)
    float du = cu - L;
    if (du >= 0.0 && du < 22.0 && abs(v - 1.0) <= 1.0 + du * 0.08) {
        float inten = (1.0 - du / 22.0) * 0.55 * gNight;
        if (bayer4(p) < inten) col = mix(col, HEADLIGHT, 0.22);
    }
    // light reflections on the wet road
    if (wRain > 0.3 && v < 0.0 && v >= -3.0) {
        float f = (1.0 + v / 4.0) * wRain;
        if (cu >= L - 2.0 && cu <= L - 1.0 && bayer4(p) < f * 0.6) col = mix(col, HEADLIGHT, 0.3);
        if (cu >= 0.0 && cu <= 1.0 && bayer4(p) < f * 0.6) col = mix(col, TAILLIGHT, 0.35);
    }
    vec4 c = carSprite(cu, v, type, hash(slot * 5.3 + lane));
    return mix(col, c.rgb, c.a);
}

vec3 neonColor(float hn) { return hn > 0.85 ? vec3(0.95, 0.35, 0.60) : (hn > 0.70 ? vec3(0.30, 0.85, 0.80) : TINT); }

vec3 sceneAvenue(vec2 p) {
    float wRain = gW.x;
    vec3 col = sky(p);
    float roadBot = max(3.0, floor(gRes.y * 0.03));      // front sidewalk ends here
    float roadTop = roadBot + 26.0;                       // road with two lanes
    float roadMid = roadBot + 13.0;
    float bTop    = roadTop + 4.0;                        // back sidewalk, then buildings

    // distant buildings
    float idF;
    float hF = bTop + building(p.x + 5.0, 12.0, 3.0, gRes.y * 0.22, gRes.y * 0.46, idF);
    if (p.y < hF) {
        col = gFar;
        vec2 w = vec2(mod(p.x + 5.0, 12.0), p.y - bTop);
        if (w.x > 1.0 && w.x < 11.0 && mod(w.x, 3.0) == 2.0 && mod(w.y, 4.0) == 2.0 && p.y < hF - 2.0) {
            float lit = hash2(vec2(idF, floor(w.y / 4.0)) + floor(w.x / 3.0) * 7.0);
            if (lit > 1.0 - 0.25 * WINDOWS * gLights) col = WINDOW_FAR;
        }
    }

    // avenue buildings: windows, ground-floor shop windows, awning and neon sign
    float idN;
    float hN = bTop + building(p.x, 22.0, 11.0, gRes.y * 0.12, gRes.y * 0.28, idN);
    if (p.y < hN && p.y >= bTop) {
        float u = mod(p.x, 22.0), y = p.y - bTop;
        col = u == 0.0 ? gNear * 0.7 : gNear;
        if (y >= 9.0 && p.y < hN - 2.0 && u > 1.0 && u < 21.0 && mod(u - 2.0, 4.0) < 2.0 && mod(y - 9.0, 5.0) < 3.0) {
            vec2 cell = vec2(idN * 10.0 + floor(u / 4.0), floor(y / 5.0));
            if (hash2(cell) > 1.0 - 0.24 * WINDOWS * gLights) col = WINDOW_NEAR;
        }
        if (y < 5.0 && u >= 2.0 && u <= 20.0) {             // ground floor
            bool door = u >= 9.0 && u <= 11.0;
            bool shop = hash(idN + 2.0) > 0.3;
            if (y >= 1.0 && !door && shop) col = mix(gNear, WINDOW_NEAR, mix(0.12, 0.55, gLights) * (y == 4.0 ? 0.7 : 1.0));
            if (door) col = gNear * 0.6;
        }
        if (y == 5.0 && u >= 1.0 && u <= 21.0 && hash(idN + 8.0) > 0.4)   // awning
            col = mix(gNear, carPaint(hash(idN + 4.0)), 0.5 + 0.2 * mod(u, 2.0));
        float hn = hash(idN + 5.0);
        float ny = 7.0 + 2.0 * floor(hash(idN + 6.0) * 4.0);              // sign height
        float nw = 4.0 + floor(hash(idN + 9.0) * 4.0);                    // half width
        if (hn > 0.55 && y >= ny && y <= ny + 1.0 && abs(u - 11.0) <= nw && p.y < hN - 2.0) {   // neon
            vec3 neon = neonColor(hn);
            float on = step(0.04, fract(gT * 0.23 + hn * 7.0));
            col = mix(gNear, neon, mix(0.35, 0.95, gNight) * on);
        }
    }

    // back sidewalk
    if (p.y >= roadTop && p.y < bTop) col = p.y == roadTop ? mix(gNear, gHorizon, 0.15) : mix(gGround, gNear, 0.55);

    // road, dashed center line and front sidewalk
    if (p.y < roadTop) {
        col = gGround;
        if (p.y == roadMid && mod(p.x, 10.0) < 5.0) col = mix(gGround, gHorizon, 0.35);
        if (wRain > 0.3 && bayer4(p) < 0.05 * wRain) col = mix(col, gHorizon, 0.25);   // wet road
    }
    if (p.y < roadBot) col = p.y == roadBot - 1.0 ? mix(gNear, gHorizon, 0.15) : mix(gGround, gNear, 0.5);

    // neon reflection on the wet sidewalk and road
    if (wRain > 0.3 && p.y >= roadTop - 3.0 && p.y < bTop) {
        float hn = hash(idN + 5.0);
        float nw = 4.0 + floor(hash(idN + 9.0) * 4.0);
        if (hn > 0.55 && abs(mod(p.x, 22.0) - 11.0) <= nw && bayer4(p) < 0.35 * wRain) col = mix(col, neonColor(hn), 0.35);
    }

    // streetlights: light falls on the road at night
    const float LAMP_GAP = 90.0, LAMP_H = 24.0;
    float lx = floor((p.x + 30.0) / LAMP_GAP) * LAMP_GAP - 30.0;
    float top = roadTop + 1.0 + LAMP_H;
    float dx = p.x - (lx + 3.5);
    if (p.y >= roadBot && p.y < top - 1.0 && abs(dx) < (top - p.y) * 0.35) {
        if (bayer4(p) < 0.12 * gNight * (1.0 - (top - p.y) / (top - roadBot) * 0.5)) col = mix(col, WINDOW_NEAR, 0.22);
    }

    // cars: far lane first (to the left), then near lane
    if (p.y >= roadMid + 2.0 && p.y < roadTop + 6.0)
        col = laneTraffic(p, col, roadMid + 3.0, -1.0, CAR_SPEED * 0.75, 1.0, wRain);
    if (p.y >= roadBot - 1.0 && p.y < roadMid + 10.0)
        col = laneTraffic(p, col, roadBot + 3.0, 1.0, CAR_SPEED, 0.0, wRain);

    // streetlight over the cars in the far lane (on the back sidewalk)
    float px = p.x - lx;
    if (px == 0.0 && p.y >= roadTop && p.y <= top) col = gNear * 0.8;
    if (p.y == top && px >= 0.0 && px <= 4.0) col = gNear * 0.8;
    if (p.y == top - 1.0 && px >= 3.0 && px <= 4.0) col = mix(gNear, WINDOW_NEAR, mix(0.3, 1.0, gLights));
    return col;
}

// ---- weather --------------------------------------------------------------
float rainLayer(vec2 p, float t, float spacing, float speed, float len, float seed) {
    vec2 q = p;
    q.x += q.y * RAIN_SLANT + t * speed * RAIN_SLANT;
    float col = floor(q.x / spacing);
    if (mod(floor(q.x), spacing) != 0.0) return 0.0;
    float h = hash(col + seed);
    if (h < 0.35) return 0.0;
    float period = 60.0 + h * 90.0;
    float d = mod(q.y + t * speed * (0.8 + 0.4 * h) + h * 500.0, period);
    return d < len ? 1.0 - d / (len * 1.4) : 0.0;
}

vec3 onehot(float i) { return vec3(i == 0.0, i == 1.0, i == 2.0); }

// weights (rain, clear, fog) with smooth transition
vec3 weatherWeights() {
    if (WEATHER >= 0.0) return onehot(WEATHER);
    float period = WEATHER_MINUTES * 60.0;
    float clock = gT + SEED;
    float idx = floor(clock / period);
    float f = gT < WEATHER_FADE ? 1.0 : smoothstep(0.0, WEATHER_FADE, mod(clock, period));
    return mix(onehot(mod(idx - 1.0, 3.0)), onehot(mod(idx, 3.0)), f);
}

// scene at this pixel, with a dithered transition between scenes
float sceneIndex(vec2 p) {
    if (SCENE >= 0.0) return SCENE;
    // a new one for each tab: the tabs' numbers count up, so the next tab gets the next one
    if (SCENE < -1.5) return mod(iSeed, SCENE_COUNT);
    float period = SCENE_MINUTES * 60.0;
    float clock = gT + SEED;
    float idx = floor(clock / period);
    float f = gT < SCENE_FADE ? 1.0 : smoothstep(0.0, SCENE_FADE, mod(clock, period));
    return bayer4(p) < f ? mod(idx, SCENE_COUNT) : mod(idx - 1.0, SCENE_COUNT);
}

// Scene color at this screen point, in sRGB. BGs is the background color (the dark tones
// come from it). Both targets call this: ghostty.glsl and lock.glsl.
vec3 sceneColor(vec2 fragCoord, vec2 resolution, float time, vec3 BGs) {
    gRes = floor(resolution / PIXEL);
    gT = time;
    vec2 p = floor(fragCoord / PIXEL);
    float uy = p.y / gRes.y;

    // Palette by time of day, always based on the theme's background (stays dark)
    if (TIME_OF_DAY == 0.0) {            // morning: grayish blue and peach horizon
        gTop     = BGs + vec3(0.10, 0.13, 0.24) * 0.55;
        gHorizon = BGs + vec3(0.60, 0.36, 0.32) * 0.40;
        gCloud   = BGs + vec3(0.55, 0.40, 0.45) * 0.35;
        gFar     = BGs + vec3(0.30, 0.26, 0.40) * 0.30;
        gMid     = BGs * 0.85 + vec3(0.12, 0.14, 0.18) * 0.35;
        gNear    = BGs * 0.65 + vec3(0.05, 0.06, 0.08) * 0.40;
        gGround  = BGs * 0.45 + vec3(0.04, 0.05, 0.05) * 0.40;
        gSun = vec3(1.00, 0.72, 0.45); gSunPos = vec2(0.20, 0.40);
        gNight = 0.0; gLights = 0.25;
    } else if (TIME_OF_DAY == 1.0) {     // afternoon: blue sky and golden horizon
        gTop     = BGs + vec3(0.12, 0.24, 0.42) * 0.55;
        gHorizon = BGs + vec3(0.50, 0.46, 0.34) * 0.40;
        gCloud   = BGs + vec3(0.50, 0.50, 0.55) * 0.38;
        gFar     = BGs + vec3(0.22, 0.30, 0.38) * 0.30;
        gMid     = BGs * 0.85 + vec3(0.10, 0.18, 0.12) * 0.40;
        gNear    = BGs * 0.65 + vec3(0.05, 0.09, 0.06) * 0.40;
        gGround  = BGs * 0.45 + vec3(0.04, 0.06, 0.04) * 0.40;
        gSun = vec3(1.00, 0.93, 0.70); gSunPos = vec2(0.70, 0.84);
        gNight = 0.0; gLights = 0.05;
    } else {                             // night: theme purple
        gTop     = BGs;
        gHorizon = BGs + TINT * TINT_AMOUNT;
        gCloud   = BGs + TINT * TINT_AMOUNT * 0.65;
        gFar     = BGs + TINT * TINT_AMOUNT * 0.35;
        gMid     = BGs * 0.8 + TINT * TINT_AMOUNT * 0.15;
        gNear    = BGs * 0.6;
        gGround  = BGs * 0.4;
        gSun = MOON; gSunPos = vec2(0.82, 0.80);
        gNight = 1.0; gLights = 1.0;
    }

    vec3 w = weatherWeights();          // x = rain, y = clear, z = fog
    gW = w;
    gCloudAmt = 1.0 - 0.75 * w.y;

    float s = sceneIndex(p);
    vec3 col;
    if      (s == 0.0) col = sceneCity(p);
    else if (s == 1.0) col = sceneHills(p);
    else if (s == 2.0) col = sceneLake(p);
    else if (s == 3.0) col = sceneCoast(p);
    else    col = sceneAvenue(p);

    // fog: slowly passing bands, denser near the ground
    if (w.z > 0.0) {
        float band = 0.55 + 0.45 * sin(p.y * 0.18 + 2.0 * sin(p.x * 0.02 + gT * 0.15) + gT * 0.1);
        float f = w.z * FOG_AMOUNT * smoothstep(0.6, 0.0, uy) * band;
        if (bayer4(p) < f) col = mix(col, gHorizon * 1.15, 0.65);
        col = mix(col, gHorizon, w.z * 0.15);
    }

    // rain and lightning
    if (w.x > 0.0) {
        float rt = gT * RAIN_SPEED;
        float rf = rainLayer(p, rt, 3.0, 55.0, 4.0, 11.0);
        float rn = rainLayer(p + vec2(1.0, 0.0), rt, 5.0, 95.0, 7.0, 47.0);
        col = mix(col, RAIN_FAR,  clamp(rf, 0.0, 1.0) * 0.55 * RAIN_OPACITY * w.x);
        col = mix(col, RAIN_NEAR, clamp(rn, 0.0, 1.0) * 0.75 * RAIN_OPACITY * w.x);
        float ft = mod(gT, 13.0);
        float flash = FLASH * w.x * (step(ft, 0.08) + 0.6 * step(0.18, ft) * step(ft, 0.24));
        col += flash * vec3(0.55, 0.5, 0.8) * (0.4 + 0.6 * uy);
    }

    col = mix(col, BGs, HAZE);
    col = mix(BGs, col, BRIGHTNESS);
    return col;
}

// ---- MiniUtils ------------------------------------------------------------------
// Each tab's turns start one scene after the tab before (iSeed counts up with each tab).
float seedValue() { return iSeed * SCENE_MINUTES * 60.0; }

// Morning from 5 to 12 h, afternoon from 12 to 18 h, night the rest (iDate.w: seconds
// since midnight, local time).
float timeOfDay() {
    if (DAYTIME >= 0.0) return DAYTIME;
    float h = iDate.w / 3600.0;
    return h >= 5.0 && h < 12.0 ? 0.0 : (h >= 12.0 && h < 18.0 ? 1.0 : 2.0);
}

// On a light theme the scene is drawn over a dark copy of the paper, then put on the
// paper: its lightness squeezed between LIGHT_SHADE of the paper (the darkest parts: the
// ground, near hills) and the paper itself (the sky, lights), with a little of its colors.
// The text, dark on such a theme, stays readable over it.
vec3 onPaper(vec3 dark, vec3 paper) {
    float l = dot(dark, vec3(0.299, 0.587, 0.114));
    vec3 hue = dark / max(l, 0.002);
    vec3 shade = mix(paper * LIGHT_SHADE, paper, clamp(l / 0.5, 0.0, 1.0));
    return clamp(shade * mix(vec3(1.0), hue, LIGHT_COLOR), 0.0, 1.0);
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec4 term = texture(iChannel0, fragCoord / iResolution.xy);
    // the background as drawn (the theme's, with the window's veil over it): of two
    // corners, the one nearer the theme's background
    vec2 px = 1.0 / iResolution.xy;
    vec3 c1 = texture(iChannel0, px * 0.5).rgb;
    vec3 c2 = texture(iChannel0, 1.0 - px * 0.5).rgb;
    vec3 bg = length(c1 - iBackgroundColor) < length(c2 - iBackgroundColor) ? c1 : c2;
    bool light = dot(bg, vec3(0.299, 0.587, 0.114)) > 0.5;
    vec3 col = light ? onPaper(sceneColor(fragCoord, iResolution.xy, iTime, bg * 0.12), bg)
                     : sceneColor(fragCoord, iResolution.xy, iTime, bg);
    // How much of this pixel is text. A glyph's edge pixels are part text, part the
    // background it was smoothed against: the strongest pixel around stands for the full
    // text color, and this one's share of it is its coverage.
    float d = length(term.rgb - bg);
    float strongest = d;
    for (int y = -1; y <= 1; y++) {
        for (int x = -1; x <= 1; x++) {
            vec3 c = texture(iChannel0, (fragCoord + vec2(float(x), float(y))) * px).rgb;
            strongest = max(strongest, length(c - bg));
        }
    }
    float cover = strongest < TEXT_CUT ? 0.0 : clamp(d / strongest, 0.0, 1.0);
    // the scene in place of the background inside the pixel too, so the edges show no dark
    // rim and the strokes keep their full weight
    fragColor = vec4(term.rgb + (col - bg) * (1.0 - cover), term.a);
}
