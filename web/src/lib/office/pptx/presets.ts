/**
 * Geometry definitions of preset shapes (a:prstGeom).
 * Written in geometry.ts's compact format from the formulas in ECMA-376 presetShapeDefinitions;
 * stars, regular polygons and line callouts are generated in code. Undefined shapes are shown as rectangles.
 */

import { parseDsl, type GeomDef } from "./geometry";

const RECT = "M l t L r t L r b L l b Z";

const ELLIPSE_TX = "g idx cos wd2 2700000; g idy sin hd2 2700000; g il +- hc 0 idx; g ir +- hc idx 0; g it +- vc 0 idy; g ib +- vc idy 0; tx il it ir ib";
const ELLIPSE_PATH = "M l vc A wd2 hd2 cd2 cd4 A wd2 hd2 3cd4 cd4 A wd2 hd2 0 cd4 A wd2 hd2 cd4 cd4 Z";

/** Arc start point (shared by arc/pie/chord/blockArc) */
const ARC_START = `
g wt1 sin wd2 stAng
g ht1 cos hd2 stAng
g dx1 cat2 wd2 ht1 wt1
g dy1 sat2 hd2 ht1 wt1
g x1 +- hc dx1 0
g y1 +- vc dy1 0`;

const ARC_ANGLES = `
g stAng pin 0 adj1 21599999
g enAng pin 0 adj2 21599999
g sw11 +- enAng 0 stAng
g sw12 +- sw11 21600000 0
g swAng ?: sw11 sw11 sw12`;

/** Callout pointer (wedgeRectCallout/wedgeRoundRectCallout) */
const WEDGE = `
av adj1 -20833
av adj2 62500
g dxPos */ w adj1 100000
g dyPos */ h adj2 100000
g xPos +- hc dxPos 0
g yPos +- vc dyPos 0
g dq */ dxPos h w
g ady abs dyPos
g adq abs dq
g dz +- ady 0 adq
g xg1 ?: dxPos 7 2
g xg2 ?: dxPos 10 5
g x1 */ w xg1 12
g x2 */ w xg2 12
g yg1 ?: dyPos 7 2
g yg2 ?: dyPos 10 5
g y1 */ h yg1 12
g y2 */ h yg2 12
g t1 ?: dxPos l xPos
g xl ?: dz l t1
g t2 ?: dyPos x1 xPos
g xt ?: dz t2 x1
g t3 ?: dxPos xPos r
g xr ?: dz r t3
g t4 ?: dyPos xPos x1
g xb ?: dz t4 x1
g t5 ?: dxPos y1 yPos
g yl ?: dz y1 t5
g t6 ?: dyPos t yPos
g yt ?: dz t6 t
g t7 ?: dxPos yPos y1
g yr ?: dz y1 t7
g t8 ?: dyPos yPos b
g yb ?: dz t8 b`;

const ROUND_RECT = `
av adj 16667
g a pin 0 adj 50000
g x1 */ ss a 100000
g x2 +- r 0 x1
g y2 +- b 0 x1
g il */ x1 29289 100000
g ir +- r 0 il
g ib +- b 0 il
M l x1 A x1 x1 cd2 cd4 L x2 t A x1 x1 3cd4 cd4 L r y2 A x1 x1 0 cd4 L x1 b A x1 x1 cd4 cd4 Z
tx il il ir ib`;

const ARROW_H = `
av adj1 50000
av adj2 50000
g maxAdj2 */ 100000 w ss
g a1 pin 0 adj1 100000
g a2 pin 0 adj2 maxAdj2
g dy1 */ h a1 200000
g y1 +- vc 0 dy1
g y2 +- vc dy1 0`;

const ARROW_V = `
av adj1 50000
av adj2 50000
g maxAdj2 */ 100000 h ss
g a1 pin 0 adj1 100000
g a2 pin 0 adj2 maxAdj2
g dx1 */ w a1 200000
g x1 +- hc 0 dx1
g x2 +- hc dx1 0`;

/** Point on the ellipse at a visual angle: x{n}/y{n} */
function ellPt(n: string, rx: string, ry: string, ang: string) {
  return `g pw${n} sin ${rx} ${ang}
g ph${n} cos ${ry} ${ang}
g pdx${n} cat2 ${rx} ph${n} pw${n}
g pdy${n} sat2 ${ry} ph${n} pw${n}
g x${n} +- hc pdx${n} 0
g y${n} +- vc pdy${n} 0`;
}

/** Arrow callouts (left/right) */
const ARROW_CALLOUT_H = `
av adj1 25000
av adj2 25000
av adj3 25000
av adj4 64977
g maxAdj2 */ 50000 h ss
g a2 pin 0 adj2 maxAdj2
g maxAdj1 */ a2 2 1
g a1 pin 0 adj1 maxAdj1
g maxAdj3 */ 100000 w ss
g a3 pin 0 adj3 maxAdj3
g q2 */ a3 ss w
g maxAdj4 +- 100000 0 q2
g a4 pin 0 adj4 maxAdj4
g dy1 */ ss a2 100000
g dy2 */ ss a1 200000
g y1 +- vc 0 dy1
g y2 +- vc 0 dy2
g y3 +- vc dy2 0
g y4 +- vc dy1 0
g dx3 */ ss a3 100000`;

/** Arrow callouts (up/down) */
const ARROW_CALLOUT_V = `
av adj1 25000
av adj2 25000
av adj3 25000
av adj4 64977
g maxAdj2 */ 50000 w ss
g a2 pin 0 adj2 maxAdj2
g maxAdj1 */ a2 2 1
g a1 pin 0 adj1 maxAdj1
g maxAdj3 */ 100000 h ss
g a3 pin 0 adj3 maxAdj3
g q2 */ a3 ss h
g maxAdj4 +- 100000 0 q2
g a4 pin 0 adj4 maxAdj4
g dx1 */ ss a2 100000
g dx2 */ ss a1 200000
g x1 +- hc 0 dx1
g x2 +- hc 0 dx2
g x3 +- hc dx2 0
g x4 +- hc dx1 0
g dy3 */ ss a3 100000`;

const SRC: Record<string, string> = {
  rect: RECT,
  roundRect: ROUND_RECT,
  ellipse: `${ELLIPSE_TX}; ${ELLIPSE_PATH}`,
  triangle: `
av adj 50000
g a pin 0 adj 100000
g x1 */ w a 200000
g x2 */ w a 100000
g x3 +- x1 wd2 0
M l b L x2 t L r b Z
tx x1 vc x3 b`,
  rtTriangle: "g it */ h 7 12; g ir */ w 7 12; g ib */ h 11 12; M l b L l t L r b Z; tx l it ir ib",
  diamond: "M l vc L hc t L r vc L hc b Z; tx wd4 hd4 3wd4 3hd4",
  parallelogram: `
av adj 25000
g maxAdj */ 100000 w ss
g a pin 0 adj maxAdj
g x2 */ ss a 100000
g x6 +- r 0 x2
g il */ wd2 a maxAdj
g q1 */ 5 a maxAdj
g q2 +/ 1 q1 12
g it */ q2 h 1
g ir +- r 0 il
g ib +- b 0 it
M l b L x2 t L r t L x6 b Z
tx il it ir ib`,
  trapezoid: `
av adj 25000
g maxAdj */ 50000 w ss
g a pin 0 adj maxAdj
g x2 */ ss a 100000
g x3 +- r 0 x2
g il */ wd3 a maxAdj
g it */ hd3 a maxAdj
g ir +- r 0 il
M l b L x2 t L x3 t L r b Z
tx il it ir b`,
  hexagon: `
av adj 25000
av vf 115470
g maxAdj */ 50000 w ss
g a pin 0 adj maxAdj
g shd2 */ hd2 vf 100000
g x1 */ ss a 100000
g x2 +- r 0 x1
g dy1 sin shd2 3600000
g y1 +- vc 0 dy1
g y2 +- vc dy1 0
M l vc L x1 y1 L x2 y1 L r vc L x2 y2 L x1 y2 Z
tx x1 y1 x2 y2`,
  octagon: `
av adj 29289
g a pin 0 adj 50000
g x1 */ ss a 100000
g x2 +- r 0 x1
g y2 +- b 0 x1
g il */ x1 1 2
g ir +- r 0 il
g ib +- b 0 il
M l x1 L x1 t L x2 t L r x1 L r y2 L x2 b L x1 b L l y2 Z
tx il il ir ib`,
  chevron: `
av adj 50000
g maxAdj */ 100000 w ss
g a pin 0 adj maxAdj
g x1 */ ss a 100000
g x2 +- r 0 x1
M l t L x2 t L r vc L x2 b L l b L x1 vc Z`,
  homePlate: `
av adj 50000
g maxAdj */ 100000 w ss
g a pin 0 adj maxAdj
g dx1 */ ss a 100000
g x1 +- r 0 dx1
g ir +/ x1 r 2
M l t L x1 t L r vc L x1 b L l b Z
tx l t ir b`,
  rightArrow: `${ARROW_H}
g dx1 */ ss a2 100000
g x1 +- r 0 dx1
g dx2 */ y1 dx1 hd2
g x2 +- x1 dx2 0
M l y1 L x1 y1 L x1 t L r vc L x1 b L x1 y2 L l y2 Z
tx l y1 x2 y2`,
  leftArrow: `${ARROW_H}
g dx2 */ ss a2 100000
g x2 +- l dx2 0
g dx1 */ y1 dx2 hd2
g x1 +- x2 0 dx1
M l vc L x2 t L x2 y1 L r y1 L r y2 L x2 y2 L x2 b Z
tx x1 y1 r y2`,
  notchedRightArrow: `${ARROW_H}
g dx2 */ ss a2 100000
g x2 +- r 0 dx2
g x1 */ dy1 dx2 hd2
M l y1 L x2 y1 L x2 t L r vc L x2 b L x2 y2 L l y2 L x1 vc Z
tx x1 y1 x2 y2`,
  stripedRightArrow: `${ARROW_H}
g dx5 */ ss a2 100000
g x5 +- r 0 dx5
g x1 */ ss 3125 100000
g x2 */ ss 1 16
g x3 */ ss 1 8
M l y1 L x1 y1 L x1 y2 L l y2 Z M x2 y1 L x3 y1 L x3 y2 L x2 y2 Z
M x3 y1 L x5 y1 L x5 t L r vc L x5 b L x5 y2 L x3 y2 Z`,
  upArrow: `${ARROW_V}
g dy2 */ ss a2 100000
g y2 +- t dy2 0
M l y2 L hc t L r y2 L x2 y2 L x2 b L x1 b L x1 y2 Z
tx x1 y2 x2 b`,
  downArrow: `${ARROW_V}
g dy1 */ ss a2 100000
g y1 +- b 0 dy1
M l y1 L x1 y1 L x1 t L x2 t L x2 y1 L r y1 L hc b Z
tx x1 t x2 y1`,
  leftRightArrow: `
av adj1 50000
av adj2 50000
g maxAdj2 */ 50000 w ss
g a1 pin 0 adj1 100000
g a2 pin 0 adj2 maxAdj2
g x2 */ ss a2 100000
g x3 +- r 0 x2
g dy */ h a1 200000
g y1 +- vc 0 dy
g y2 +- vc dy 0
M l vc L x2 t L x2 y1 L x3 y1 L x3 t L r vc L x3 b L x3 y2 L x2 y2 L x2 b Z
tx x2 y1 x3 y2`,
  upDownArrow: `
av adj1 50000
av adj2 50000
g maxAdj2 */ 50000 h ss
g a1 pin 0 adj1 100000
g a2 pin 0 adj2 maxAdj2
g y2 */ ss a2 100000
g y3 +- b 0 y2
g dx1 */ w a1 200000
g x1 +- hc 0 dx1
g x2 +- hc dx1 0
M l y2 L hc t L r y2 L x2 y2 L x2 y3 L r y3 L hc b L l y3 L x1 y3 L x1 y2 Z
tx x1 y2 x2 y3`,
  quadArrow: `
av adj1 22500
av adj2 22500
av adj3 22500
g a2 pin 0 adj2 50000
g maxAdj1 */ a2 2 1
g a1 pin 0 adj1 maxAdj1
g q1 +- 100000 0 maxAdj1
g maxAdj3 */ q1 1 2
g a3 pin 0 adj3 maxAdj3
g x1 */ ss a3 100000
g dx2 */ ss a2 100000
g x2 +- hc 0 dx2
g x5 +- hc dx2 0
g dx3 */ ss a1 200000
g x3 +- hc 0 dx3
g x4 +- hc dx3 0
g x6 +- r 0 x1
g y2 +- vc 0 dx2
g y5 +- vc dx2 0
g y3 +- vc 0 dx3
g y4 +- vc dx3 0
g y6 +- b 0 x1
M l vc L x1 y2 L x1 y3 L x3 y3 L x3 x1 L x2 x1 L hc t L x5 x1 L x4 x1 L x4 y3 L x6 y3 L x6 y2 L r vc L x6 y5 L x6 y4 L x4 y4 L x4 y6 L x5 y6 L hc b L x2 y6 L x3 y6 L x3 y4 L x1 y4 L x1 y5 Z`,
  bentArrow: `
av adj1 25000
av adj2 25000
av adj3 25000
av adj4 43750
g a2 pin 0 adj2 50000
g maxAdj1 */ a2 2 1
g a1 pin 0 adj1 maxAdj1
g a3 pin 0 adj3 50000
g th */ ss a1 100000
g aw2 */ ss a2 100000
g th2 */ th 1 2
g dh2 +- aw2 0 th2
g ah */ ss a3 100000
g bw +- r 0 ah
g bh +- b 0 dh2
g bs min bw bh
g maxAdj4 */ 100000 bs ss
g a4 pin 0 adj4 maxAdj4
g bd */ ss a4 100000
g bd3 +- bd 0 th
g bd2 max bd3 0
g x3 +- th bd2 0
g x4 +- r 0 ah
g y3 +- dh2 th 0
g y4 +- y3 dh2 0
g y5 +- dh2 bd 0
g y6 +- y3 bd2 0
M l b L l y5 A bd bd cd2 cd4 L x4 dh2 L x4 t L r aw2 L x4 y4 L x4 y3 L x3 y3 A bd2 bd2 3cd4 -5400000 L th b Z`,
  uturnArrow: `
av adj1 25000
av adj2 25000
av adj3 25000
av adj4 43750
av adj5 75000
g a2 pin 0 adj2 25000
g maxAdj1 */ a2 2 1
g a1 pin 0 adj1 maxAdj1
g q2 */ adj5 ss h
g q3 +- 100000 0 q2
g maxAdj3 */ q3 w ss
g a3 pin 0 adj3 maxAdj3
g q1 +- a3 a1 0
g minAdj5 */ q1 ss h
g a5 pin minAdj5 adj5 100000
g th */ ss a1 100000
g aw2 */ ss a2 100000
g th2 */ th 1 2
g dh2 +- aw2 0 th2
g y5 */ h a5 100000
g ah */ ss a3 100000
g y4 +- y5 0 ah
g x9 +- r 0 dh2
g bw */ x9 1 2
g bs min bw y4
g maxAdj4 */ bs 100000 ss
g a4 pin 0 adj4 maxAdj4
g bd */ ss a4 100000
g bd3 +- bd 0 th
g bd2 max bd3 0
g x3 +- th bd2 0
g x8 +- r 0 aw2
g x6 +- x8 0 aw2
g x7 +- x6 dh2 0
g x4 +- x9 0 bd
g x5 +- x7 0 bd2
M l b L l bd A bd bd cd2 cd4 L x4 t A bd bd 3cd4 cd4 L x9 y4 L r y4 L x8 y5 L x6 y4 L x7 y4 L x7 x3 A bd2 bd2 0 -5400000 L x3 th A bd2 bd2 3cd4 -5400000 L th b Z`,
  plus: `
av adj 25000
g a pin 0 adj 50000
g x1 */ ss a 100000
g x2 +- r 0 x1
g y2 +- b 0 x1
M l x1 L x1 x1 L x1 t L x2 t L x2 x1 L r x1 L r y2 L x2 y2 L x2 b L x1 b L x1 y2 L l y2 Z
tx l x1 r y2`,
  donut: `
av adj 25000
g a pin 0 adj 50000
g dr */ ss a 100000
g iwd2 +- wd2 0 dr
g ihd2 +- hd2 0 dr
${ELLIPSE_PATH}
M dr vc A iwd2 ihd2 cd2 -5400000 A iwd2 ihd2 cd4 -5400000 A iwd2 ihd2 0 -5400000 A iwd2 ihd2 3cd4 -5400000 Z
${ELLIPSE_TX}`,
  noSmoking: `
av adj 18750
g a pin 0 adj 50000
g dr */ ss a 100000
g iwd2 +- wd2 0 dr
g ihd2 +- hd2 0 dr
g ang at2 w h
g ct cos ihd2 ang
g st sin iwd2 ang
g m mod ct st 0
g n */ iwd2 ihd2 m
g drd2 */ dr 1 2
g dang at2 n drd2
g dang2 */ dang 2 1
g swAng +- -10800000 dang2 0
g t3 at2 w h
g stAng1 +- t3 0 dang
g stAng2 +- stAng1 0 cd2
g ct1 cos ihd2 stAng1
g st1 sin iwd2 stAng1
g m1 mod ct1 st1 0
g n1 */ iwd2 ihd2 m1
g dx1 cos n1 stAng1
g dy1 sin n1 stAng1
g x1 +- hc dx1 0
g y1 +- vc dy1 0
g x2 +- hc 0 dx1
g y2 +- vc 0 dy1
${ELLIPSE_PATH}
M x1 y1 A iwd2 ihd2 stAng1 swAng Z
M x2 y2 A iwd2 ihd2 stAng2 swAng Z`,
  arc: `
av adj1 16200000
av adj2 0
${ARC_ANGLES}
${ARC_START}
p nostroke
M x1 y1 A wd2 hd2 stAng swAng L hc vc Z
p none
M x1 y1 A wd2 hd2 stAng swAng`,
  pie: `
av adj1 0
av adj2 16200000
${ARC_ANGLES}
${ARC_START}
M x1 y1 A wd2 hd2 stAng swAng L hc vc Z`,
  chord: `
av adj1 2700000
av adj2 16200000
${ARC_ANGLES}
${ARC_START}
M x1 y1 A wd2 hd2 stAng swAng Z`,
  blockArc: `
av adj1 10800000
av adj2 0
av adj3 25000
g stAng pin 0 adj1 21599999
g istAng pin 0 adj2 21599999
g a3 pin 0 adj3 50000
g sw11 +- istAng 0 stAng
g sw12 +- sw11 21600000 0
g swAng ?: sw11 sw11 sw12
g iswAng +- 0 0 swAng
${ARC_START}
g dr */ ss a3 100000
g iwd2 +- wd2 0 dr
g ihd2 +- hd2 0 dr
g wt3 sin iwd2 istAng
g ht3 cos ihd2 istAng
g dx3 cat2 iwd2 ht3 wt3
g dy3 sat2 ihd2 ht3 wt3
g x3 +- hc dx3 0
g y3 +- vc dy3 0
M x1 y1 A wd2 hd2 stAng swAng L x3 y3 A iwd2 ihd2 istAng iswAng Z`,
  pieWedge: "M l b A w h cd2 cd4 L r b Z",
  wedgeRectCallout: `${WEDGE}
M l t L x1 t L xt yt L x2 t L r t L r y1 L xr yr L r y2 L r b L x2 b L xb yb L x1 b L l b L l y2 L xl yl L l y1 Z`,
  wedgeRoundRectCallout: `${WEDGE}
av adj3 16667
g u1 */ ss adj3 100000
g u2 +- r 0 u1
g v2 +- b 0 u1
g il */ u1 29289 100000
g ir +- r 0 il
g ib +- b 0 il
M l u1 A u1 u1 cd2 cd4 L x1 t L xt yt L x2 t L u2 t A u1 u1 3cd4 cd4 L r y1 L xr yr L r y2 L r v2 A u1 u1 0 cd4 L x2 b L xb yb L x1 b L u1 b A u1 u1 cd4 cd4 L l y2 L xl yl L l y1 Z
tx il il ir ib`,
  wedgeEllipseCallout: `
av adj1 -20833
av adj2 62500
g dxPos */ w adj1 100000
g dyPos */ h adj2 100000
g xPos +- hc dxPos 0
g yPos +- vc dyPos 0
g sdx */ dxPos h 1
g sdy */ dyPos w 1
g pang at2 sdx sdy
g stAng +- pang 660000 0
g enAng +- pang 0 660000
g dx1 cos wd2 stAng
g dy1 sin hd2 stAng
g x1 +- hc dx1 0
g y1 +- vc dy1 0
g dx2 cos wd2 enAng
g dy2 sin hd2 enAng
g x2 +- hc dx2 0
g y2 +- vc dy2 0
g stAng1 at2 dx1 dy1
g enAng1 at2 dx2 dy2
g swAng1 +- enAng1 0 stAng1
g swAng2 +- swAng1 21600000 0
g swAng ?: swAng1 swAng1 swAng2
M x2 y2 L xPos yPos L x1 y1 A wd2 hd2 stAng1 swAng Z
${ELLIPSE_TX}`,
  cloud: `
p w=43200 h=43200
M 3900 14370 A 6753 9190 -11429249 7426832 A 5333 7267 -8646143 5396714 A 4365 5945 -8748475 5983381 A 4857 6595 -7859164 7034504 A 5333 7273 -4722533 6541615 A 6775 9220 -2776035 7816140 A 5785 7867 37501 6842000 A 6752 9215 1347096 6910353 A 7720 10543 3974558 4542661 A 4360 5918 -16496525 8804134 A 4345 5945 -14809710 9151131 Z
tx 0 0 w h`,
  heart: `
g dx1 */ w 49 48
g dx2 */ w 10 48
g x1 +- hc 0 dx1
g x2 +- hc 0 dx2
g x3 +- hc dx2 0
g x4 +- hc dx1 0
g y1 +- t 0 hd3
M hc hd4 C x3 y1 x4 hd4 hc b C x1 hd4 x2 y1 hc hd4 Z
tx wd4 hd4 3wd4 3hd4`,
  lightningBolt: `
p w=21600 h=21600
M 8472 0 L 12860 6080 L 11050 6797 L 16577 12007 L 14767 12877 L 21600 21600 L 10012 14915 L 12222 13987 L 5022 9705 L 7602 8382 L 0 3890 Z`,
  can: `
av adj 25000
g maxAdj */ 50000 h ss
g a pin 0 adj maxAdj
g y1 */ ss a 200000
g y2 +- y1 y1 0
g y3 +- b 0 y1
p nostroke
M l y1 A wd2 y1 cd2 -10800000 L r y3 A wd2 y1 0 cd2 Z
p lighten nostroke
M l y1 A wd2 y1 cd2 cd2 A wd2 y1 0 cd2 Z
p none
M r y1 A wd2 y1 0 cd2 A wd2 y1 cd2 cd2 L r y3 A wd2 y1 0 cd2 L l y1
tx l y2 r y3`,
  cube: `
av adj 25000
g a pin 0 adj 100000
g y1 */ ss a 100000
g y4 +- b 0 y1
g x4 +- r 0 y1
p nostroke
M l y1 L x4 y1 L x4 b L l b Z
p darkenLess nostroke
M x4 y1 L r t L r y4 L x4 b Z
p lightenLess nostroke
M l y1 L y1 t L r t L x4 y1 Z
p none
M l y1 L y1 t L r t L r y4 L x4 b L l b Z M l y1 L x4 y1 L r t M x4 y1 L x4 b
tx l y1 x4 b`,
  bevel: `
av adj 12500
g a pin 0 adj 50000
g x1 */ ss a 100000
g x2 +- r 0 x1
g y2 +- b 0 x1
p nostroke
M x1 x1 L x2 x1 L x2 y2 L x1 y2 Z
p lightenLess nostroke
M l t L r t L x2 x1 L x1 x1 Z
p darkenLess nostroke
M r t L r b L x2 y2 L x2 x1 Z
p darken nostroke
M l b L x1 y2 L x2 y2 L r b Z
p lighten nostroke
M l t L x1 x1 L x1 y2 L l b Z
p none
M l t L r t L r b L l b Z M x1 x1 L x2 x1 L x2 y2 L x1 y2 Z M l t L x1 x1 M l b L x1 y2 M r t L x2 x1 M r b L x2 y2
tx x1 x1 x2 y2`,
  frame: `
av adj1 12500
g a1 pin 0 adj1 50000
g x1 */ ss a1 100000
g x4 +- r 0 x1
g y4 +- b 0 x1
M l t L r t L r b L l b Z M x1 x1 L x1 y4 L x4 y4 L x4 x1 Z
tx x1 x1 x4 y4`,
  halfFrame: `
av adj1 33333
av adj2 33333
g x1 */ ss adj2 100000
g y1 */ ss adj1 100000
g dx2 */ y1 w h
g x2 +- r 0 dx2
g dy2 */ x1 h w
g y2 +- b 0 dy2
M l t L r t L x2 y1 L x1 y1 L x1 y2 L l b Z`,
  corner: `
av adj1 50000
av adj2 50000
g maxAdj1 */ 100000 h ss
g maxAdj2 */ 100000 w ss
g a1 pin 0 adj1 maxAdj1
g a2 pin 0 adj2 maxAdj2
g x1 */ ss a2 100000
g dy1 */ ss a1 100000
g y1 +- b 0 dy1
M l t L x1 t L x1 y1 L r y1 L r b L l b Z`,
  diagStripe: `
av adj 50000
g a pin 0 adj 100000
g x2 */ w a 100000
g y2 */ h a 100000
M l y2 L x2 t L r t L l b Z`,
  plaque: `
av adj 16667
g a pin 0 adj 50000
g x1 */ ss a 100000
g x2 +- r 0 x1
g y2 +- b 0 x1
g il */ x1 70711 100000
g ir +- r 0 il
g ib +- b 0 il
M l x1 A x1 x1 cd4 -5400000 L x2 t A x1 x1 cd2 -5400000 L r y2 A x1 x1 3cd4 -5400000 L x1 b A x1 x1 0 -5400000 Z
tx il il ir ib`,
  foldedCorner: `
av adj 16667
g a pin 0 adj 50000
g dy2 */ ss a 100000
g dy1 */ dy2 1 5
g x1 +- r 0 dy2
g x2 +- x1 dy1 0
g y2 +- b 0 dy2
g y1 +- y2 dy1 0
p nostroke
M l t L r t L r y2 L x1 b L l b Z
p darkenLess nostroke
M x1 b L x2 y1 L r y2 Z
p none
M x1 b L x2 y1 L r y2 L x1 b L l b L l t L r t L r y2
tx l t r y2`,
  teardrop: `
av adj 100000
g a pin 0 adj 200000
g dx */ wd2 a 100000
g dy */ hd2 a 100000
g x1 +- hc dx 0
g y1 +- vc 0 dy
g cx1 +/ hc x1 2
g cy1 +/ t y1 2
g cx2 +/ r x1 2
g cy2 +/ vc y1 2
M l vc A wd2 hd2 cd2 cd4 Q cx1 cy1 x1 y1 Q cx2 cy2 r vc A wd2 hd2 0 cd2 Z
${ELLIPSE_TX}`,
  smileyFace: `
av adj 4653
g a pin -4653 adj 4653
g x2 */ w 6215 21600
g x3 */ w 13135 21600
g x4 */ w 16640 21600
g y1 */ h 7570 21600
g y3 */ h 16515 21600
g dy2 */ h a 100000
g y2 +- y3 0 dy2
g y4 +- y3 dy2 0
g dy3 */ h a 50000
g y5 +- y4 dy3 0
g wR */ w 1125 21600
g hR */ h 1125 21600
g x1 */ w 4960 21600
g ex1 +- x2 0 wR
g ex2 +- x3 0 wR
${ELLIPSE_PATH}
p darkenLess
M ex1 y1 A wR hR cd2 21600000 Z M ex2 y1 A wR hR cd2 21600000 Z
p none
M x1 y2 Q hc y5 x4 y2
${ELLIPSE_TX}`,
  leftBracket: `
av adj 8333
g maxAdj */ 50000 h ss
g a pin 0 adj maxAdj
g y1 */ ss a 100000
g y2 +- b 0 y1
p nostroke
M r b A w y1 cd4 cd4 L l y1 A w y1 cd2 cd4 Z
p none
M r b A w y1 cd4 cd4 L l y1 A w y1 cd2 cd4`,
  rightBracket: `
av adj 8333
g maxAdj */ 50000 h ss
g a pin 0 adj maxAdj
g y1 */ ss a 100000
g y2 +- b 0 y1
p nostroke
M l t A w y1 3cd4 cd4 L r y2 A w y1 0 cd4 Z
p none
M l t A w y1 3cd4 cd4 L r y2 A w y1 0 cd4`,
  leftBrace: `
av adj1 8333
av adj2 50000
g a2 pin 0 adj2 100000
g q1 +- 100000 0 a2
g q2 min q1 a2
g q3 */ q2 1 2
g maxAdj1 */ q3 h ss
g a1 pin 0 adj1 maxAdj1
g y1 */ ss a1 100000
g y3 */ h a2 100000
g y2 +- y3 0 y1
g ya +- y3 y1 0
p nostroke
M r b A wd2 y1 cd4 cd4 L hc ya A wd2 y1 0 -5400000 A wd2 y1 cd4 -5400000 L hc y1 A wd2 y1 cd2 cd4 Z
p none
M r b A wd2 y1 cd4 cd4 L hc ya A wd2 y1 0 -5400000 A wd2 y1 cd4 -5400000 L hc y1 A wd2 y1 cd2 cd4`,
  rightBrace: `
av adj1 8333
av adj2 50000
g a2 pin 0 adj2 100000
g q1 +- 100000 0 a2
g q2 min q1 a2
g q3 */ q2 1 2
g maxAdj1 */ q3 h ss
g a1 pin 0 adj1 maxAdj1
g y1 */ ss a1 100000
g y3 */ h a2 100000
g y2 +- y3 0 y1
g y4 +- b 0 y1
p nostroke
M l t A wd2 y1 3cd4 cd4 L hc y2 A wd2 y1 cd2 -5400000 A wd2 y1 3cd4 -5400000 L hc y4 A wd2 y1 0 cd4 Z
p none
M l t A wd2 y1 3cd4 cd4 L hc y2 A wd2 y1 cd2 -5400000 A wd2 y1 3cd4 -5400000 L hc y4 A wd2 y1 0 cd4`,
  bracketPair: `
av adj 16667
g a pin 0 adj 50000
g x1 */ ss a 100000
g x2 +- r 0 x1
g y2 +- b 0 x1
g il */ x1 29289 100000
g ir +- r 0 il
g ib +- b 0 il
p nostroke
M l x1 A x1 x1 cd2 cd4 L x2 t A x1 x1 3cd4 cd4 L r y2 A x1 x1 0 cd4 L x1 b A x1 x1 cd4 cd4 Z
p none
M x1 b A x1 x1 cd4 cd4 L l x1 A x1 x1 cd2 cd4 M x2 t A x1 x1 3cd4 cd4 L r y2 A x1 x1 0 cd4
tx il il ir ib`,
  bracePair: `
av adj 8333
g a pin 0 adj 25000
g x1 */ ss a 100000
g x2 */ ss a 50000
g x3 +- r 0 x2
g x4 +- r 0 x1
g y2 +- vc 0 x1
g y3 +- vc x1 0
g y4 +- b 0 x1
p nostroke
M x2 b A x1 x1 cd4 cd4 L x1 y3 A x1 x1 0 -5400000 A x1 x1 cd4 -5400000 L x1 x1 A x1 x1 cd2 cd4 L x3 t A x1 x1 3cd4 cd4 L x4 y2 A x1 x1 cd2 -5400000 A x1 x1 3cd4 -5400000 L x4 y4 A x1 x1 0 cd4 Z
p none
M x2 b A x1 x1 cd4 cd4 L x1 y3 A x1 x1 0 -5400000 A x1 x1 cd4 -5400000 L x1 x1 A x1 x1 cd2 cd4 M x3 t A x1 x1 3cd4 cd4 L x4 y2 A x1 x1 cd2 -5400000 A x1 x1 3cd4 -5400000 L x4 y4 A x1 x1 0 cd4`,
  snip1Rect: `
av adj 16667
g a pin 0 adj 50000
g dx1 */ ss a 100000
g x1 +- r 0 dx1
g it */ dx1 1 2
g ir +/ x1 r 2
M l t L x1 t L r dx1 L r b L l b Z
tx l it ir b`,
  snip2SameRect: `
av adj1 16667
av adj2 0
g a1 pin 0 adj1 50000
g a2 pin 0 adj2 50000
g tx1 */ ss a1 100000
g tx2 +- r 0 tx1
g bx1 */ ss a2 100000
g bx2 +- r 0 bx1
g by1 +- b 0 bx1
M tx1 t L tx2 t L r tx1 L r by1 L bx2 b L bx1 b L l by1 L l tx1 Z`,
  snip2DiagRect: `
av adj1 0
av adj2 16667
g a1 pin 0 adj1 50000
g a2 pin 0 adj2 50000
g lx1 */ ss a1 100000
g lx2 +- r 0 lx1
g ly1 +- b 0 lx1
g rx1 */ ss a2 100000
g rx2 +- r 0 rx1
g ry1 +- b 0 rx1
M lx1 t L rx2 t L r rx1 L r ly1 L lx2 b L rx1 b L l ry1 L l lx1 Z`,
  snipRoundRect: `
av adj1 16667
av adj2 16667
g a1 pin 0 adj1 50000
g a2 pin 0 adj2 50000
g x1 */ ss a1 100000
g dx2 */ ss a2 100000
g x2 +- r 0 dx2
M x1 t L x2 t L r dx2 L r b L l b L l x1 A x1 x1 cd2 cd4 Z`,
  round1Rect: `
av adj 16667
g a pin 0 adj 50000
g dx1 */ ss a 100000
g x1 +- r 0 dx1
M l t L x1 t A dx1 dx1 3cd4 cd4 L r b L l b Z`,
  round2SameRect: `
av adj1 16667
av adj2 0
g a1 pin 0 adj1 50000
g a2 pin 0 adj2 50000
g tx1 */ ss a1 100000
g tx2 +- r 0 tx1
g bx1 */ ss a2 100000
g bx2 +- r 0 bx1
g by1 +- b 0 bx1
M tx1 t L tx2 t A tx1 tx1 3cd4 cd4 L r by1 A bx1 bx1 0 cd4 L bx1 b A bx1 bx1 cd4 cd4 L l tx1 A tx1 tx1 cd2 cd4 Z`,
  round2DiagRect: `
av adj1 16667
av adj2 0
g a1 pin 0 adj1 50000
g a2 pin 0 adj2 50000
g x1 */ ss a1 100000
g y1 +- b 0 x1
g a */ ss a2 100000
g x2 +- r 0 a
g y2 +- b 0 a
M x1 t L x2 t A a a 3cd4 cd4 L r y1 A x1 x1 0 cd4 L a b A a a cd4 cd4 L l x1 A x1 x1 cd2 cd4 Z`,
  mathPlus: `
av adj1 23520
g a1 pin 0 adj1 73490
g dx1 */ w 73490 200000
g dy1 */ h 73490 200000
g dx2 */ ss a1 200000
g x1 +- hc 0 dx1
g x2 +- hc 0 dx2
g x3 +- hc dx2 0
g x4 +- hc dx1 0
g y1 +- vc 0 dy1
g y2 +- vc 0 dx2
g y3 +- vc dx2 0
g y4 +- vc dy1 0
M x1 y2 L x2 y2 L x2 y1 L x3 y1 L x3 y2 L x4 y2 L x4 y3 L x3 y3 L x3 y4 L x2 y4 L x2 y3 L x1 y3 Z`,
  mathMinus: `
av adj1 23520
g a1 pin 0 adj1 100000
g dy1 */ h a1 200000
g dx1 */ w 73490 200000
g y1 +- vc 0 dy1
g y2 +- vc dy1 0
g x1 +- hc 0 dx1
g x2 +- hc dx1 0
M x1 y1 L x2 y1 L x2 y2 L x1 y2 Z`,
  mathEqual: `
av adj1 23520
av adj2 11760
g a1 pin 0 adj1 36745
g a2 pin 0 adj2 73490
g dy1 */ h a1 100000
g dy2 */ h a2 200000
g dx1 */ w 73490 200000
g y2 +- vc 0 dy2
g y3 +- vc dy2 0
g y1 +- y2 0 dy1
g y4 +- y3 dy1 0
g x1 +- hc 0 dx1
g x2 +- hc dx1 0
M x1 y1 L x2 y1 L x2 y2 L x1 y2 Z M x1 y3 L x2 y3 L x2 y4 L x1 y4 Z`,
  wave: `
av adj1 12500
av adj2 0
g a1 pin 0 adj1 20000
g y1 */ h a1 100000
g dy2 */ y1 10 3
g y2 +- y1 0 dy2
g y3 +- y1 dy2 0
g y4 +- b 0 y1
g y5 +- y4 0 dy2
g y6 +- y4 dy2 0
g x2 */ w 1 3
g x3 */ w 2 3
M l y1 C x2 y2 x3 y3 r y1 L r y4 C x3 y6 x2 y5 l y4 Z
tx l y1 r y4`,
  doubleWave: `
av adj1 6250
av adj2 0
g a1 pin 0 adj1 12500
g y1 */ h a1 100000
g dy2 */ y1 10 3
g y2 +- y1 0 dy2
g y3 +- y1 dy2 0
g y4 +- b 0 y1
g y5 +- y4 0 dy2
g y6 +- y4 dy2 0
g x1 */ w 1 6
g x2 */ w 1 3
g x3 */ w 1 2
g x4 */ w 2 3
g x5 */ w 5 6
M l y1 C x1 y2 x2 y3 x3 y1 C x4 y2 x5 y3 r y1 L r y4 C x5 y6 x4 y5 x3 y4 C x2 y6 x1 y5 l y4 Z
tx l y1 r y4`,
  irregularSeal1: `
p w=21600 h=21600
M 10800 5800 L 14522 0 L 14155 5325 L 18380 4457 L 16702 7315 L 21097 8137 L 17607 10475 L 21600 13290 L 16837 12942 L 18145 18095 L 14020 14457 L 13247 19737 L 10532 14935 L 8485 21600 L 7715 15627 L 4762 17617 L 5667 13937 L 135 14587 L 3722 11775 L 0 8615 L 4627 7617 L 370 2295 L 7312 6320 L 8352 2295 Z
tx wd4 hd4 3wd4 3hd4`,
  irregularSeal2: `
p w=21600 h=21600
M 11462 4342 L 14790 0 L 14525 5777 L 18007 3172 L 16380 6532 L 21600 6645 L 16985 9402 L 18270 11290 L 16380 12310 L 18877 15632 L 14640 14350 L 14942 17370 L 12180 15935 L 11612 18842 L 9872 17370 L 8700 19712 L 7527 18125 L 4917 21600 L 4805 18240 L 1285 17825 L 3330 15370 L 0 12877 L 3935 11592 L 1172 8270 L 5372 7817 L 4502 3625 L 8550 6382 L 9722 1887 Z
tx wd4 hd4 3wd4 3hd4`,
  flowChartTerminator: `
p w=21600 h=21600
M 3475 0 L 18125 0 A 3475 10800 3cd4 cd2 L 3475 21600 A 3475 10800 cd4 cd2 Z
g il */ w 1018 21600
g ir */ w 20582 21600
g it */ h 3163 21600
g ib */ h 18437 21600
tx il it ir ib`,
  flowChartDocument: `
p w=21600 h=21600
M 0 0 L 21600 0 L 21600 17322 C 10800 17322 10800 23922 0 20172 Z
g y2 */ h 17322 21600
tx l t r y2`,
  flowChartMultidocument: `
p w=21600 h=21600
M 0 20172 C 8680 22280 9425 18145 18300 18145 L 18300 3665 L 0 3665 Z
p none w=21600 h=21600
M 1532 3665 L 1532 1799 L 19890 1799 L 19890 16277 C 19614 16280 19298 16329 18300 16426 M 3029 1799 L 3029 0 L 21600 0 L 21600 14311 C 20936 14295 20541 14374 19890 14584
p nostroke w=21600 h=21600
M 1532 3665 L 1532 1799 L 19890 1799 L 19890 16277 C 19614 16280 19298 16329 18300 16426 L 18300 3665 Z M 3029 1799 L 3029 0 L 21600 0 L 21600 14311 C 20936 14295 20541 14374 19890 14584 L 19890 1799 Z
g y2 */ h 20782 21600
g y8 */ h 3665 21600
g x5 */ w 18300 21600
tx l y8 x5 y2`,
  flowChartPredefinedProcess: `
p w=1 h=1
M 0 0 L 1 0 L 1 1 L 0 1 Z
p none w=8 h=8
M 1 0 L 1 8 M 7 0 L 7 8
g x2 */ w 1 8
g x3 */ w 7 8
tx x2 t x3 b`,
  flowChartInternalStorage: `
p w=1 h=1
M 0 0 L 1 0 L 1 1 L 0 1 Z
p none w=8 h=8
M 1 0 L 1 8 M 0 1 L 8 1
g x1 */ w 1 8
g y1 */ h 1 8
tx x1 y1 r b`,
  flowChartInputOutput: `
p w=5 h=5
M 0 5 L 1 0 L 5 0 L 4 5 Z
g x3 */ w 2 5
g x4 */ w 3 5
g x5 */ w 4 5
g x6 */ w 1 5
tx x6 t x5 b`,
  flowChartManualInput: "p w=5 h=5; M 0 1 L 5 0 L 5 5 L 0 5 Z; g it */ h 1 5; tx l it r b",
  flowChartManualOperation: "p w=5 h=5; M 0 0 L 5 0 L 4 5 L 1 5 Z; g x3 */ w 4 5; g x4 */ w 1 5; tx x4 t x3 b",
  flowChartPreparation: "p w=10 h=10; M 0 5 L 2 0 L 8 0 L 10 5 L 8 10 L 2 10 Z; g x2 */ w 4 5; tx wd5 t x2 b",
  flowChartOffpageConnector: "p w=10 h=10; M 0 0 L 10 0 L 10 8 L 5 10 L 0 8 Z; g y1 */ h 4 5; tx l t r y1",
  flowChartPunchedCard: "p w=5 h=5; M 0 1 L 1 0 L 5 0 L 5 5 L 0 5 Z; g it */ h 1 5; tx l it r b",
  flowChartExtract: "p w=2 h=2; M 0 2 L 1 0 L 2 2 Z; g x1 */ w 1 4; g x2 */ w 3 4; g y2 */ h 1 2; tx x1 y2 x2 b",
  flowChartMerge: "p w=2 h=2; M 0 0 L 2 0 L 1 2 Z; g x1 */ w 1 4; g x2 */ w 3 4; g y2 */ h 1 2; tx x1 t x2 y2",
  flowChartCollate: "p w=2 h=2; M 0 0 L 2 0 L 1 1 L 2 2 L 0 2 L 1 1 Z; g ir */ w 3 4; g ib */ h 3 4; tx wd4 hd4 ir ib",
  flowChartSort: "p w=2 h=2; M 0 1 L 1 0 L 2 1 L 1 2 Z; p none w=2 h=2; M 0 1 L 2 1; g ir */ w 3 4; g ib */ h 3 4; tx wd4 hd4 ir ib",
  flowChartDelay: `M l t L hc t A wd2 hd2 3cd4 cd2 L l b Z; ${ELLIPSE_TX.replace("tx il it ir ib", "tx l it ir ib")}`,
  flowChartDisplay: "p w=6 h=6; M 0 3 L 1 0 L 5 0 A 1 3 3cd4 cd2 L 1 6 Z; g x2 */ w 5 6; tx wd6 t x2 b",
  flowChartOnlineStorage: "p w=6 h=6; M 1 0 L 6 0 A 1 3 3cd4 -10800000 L 1 6 A 1 3 cd4 cd2 Z; g x2 */ w 5 6; tx wd6 t x2 b",
  flowChartMagneticDisk: `
p w=6 h=6
M 0 1 A 3 1 cd2 cd2 L 6 5 A 3 1 0 cd2 Z
p none w=6 h=6
M 6 1 A 3 1 0 cd2
g y3 */ h 1 3
g y5 */ h 5 6
tx l y3 r y5`,
  flowChartMagneticDrum: `
p w=6 h=6
M 1 0 L 5 0 A 1 3 3cd4 cd2 L 1 6 A 1 3 cd4 cd2 Z
p none w=6 h=6
M 5 6 A 1 3 cd4 cd2
g x1 */ w 1 3
g x2 */ w 2 3
tx x1 t x2 b`,
  flowChartPunchedTape: `
p w=20 h=20
M 0 2 A 5 2 cd2 -10800000 A 5 2 cd2 cd2 L 20 18 A 5 2 0 -10800000 A 5 2 0 cd2 Z
g y2 */ h 1 5
g ib */ h 4 5
tx l y2 r ib`,
  flowChartOr: `${ELLIPSE_PATH}; p none; M hc t L hc b M l vc L r vc; ${ELLIPSE_TX}`,
  flowChartSummingJunction: `${ELLIPSE_TX}; ${ELLIPSE_PATH}; p none; M il it L ir ib M ir it L il ib`,
  moon: `
av adj 50000
g a pin 0 adj 87500
g g0 */ ss a 100000
g g0w */ g0 w ss
g iw +- w 0 g0w
M r b A w hd2 cd4 cd2 A iw hd2 3cd4 -10800000 Z`,
  nonIsoscelesTrapezoid: `
av adj1 25000
av adj2 25000
g maxAdj */ 50000 w ss
g a1 pin 0 adj1 maxAdj
g a2 pin 0 adj2 maxAdj
g x1 */ ss a1 100000
g dx3 */ ss a2 100000
g x2 +- r 0 dx3
M l b L x1 t L x2 t L r b Z`,
  rightArrowCallout: `${ARROW_CALLOUT_H}
g x3 +- r 0 dx3
g x2 */ w a4 100000
M l t L x2 t L x2 y2 L x3 y2 L x3 y1 L r vc L x3 y4 L x3 y3 L x2 y3 L x2 b L l b Z
tx l t x2 b`,
  leftArrowCallout: `${ARROW_CALLOUT_H}
g x1 +- l dx3 0
g dx2 */ w a4 100000
g x2 +- r 0 dx2
M l vc L x1 y1 L x1 y2 L x2 y2 L x2 t L r t L r b L x2 b L x2 y3 L x1 y3 L x1 y4 Z
tx x2 t r b`,
  downArrowCallout: `${ARROW_CALLOUT_V}
g y3 +- b 0 dy3
g y2 */ h a4 100000
M l t L r t L r y2 L x3 y2 L x3 y3 L x4 y3 L hc b L x1 y3 L x2 y3 L x2 y2 L l y2 Z
tx l t r y2`,
  upArrowCallout: `${ARROW_CALLOUT_V}
g y1 +- t dy3 0
g dy2 */ h a4 100000
g y2 +- b 0 dy2
M l y2 L x2 y2 L x2 y1 L x1 y1 L hc t L x4 y1 L x3 y1 L x3 y2 L r y2 L r b L l b Z
tx l y2 r b`,
  circularArrow: `
av adj1 12500
av adj2 1142319
av adj3 20457681
av adj4 10800000
av adj5 12500
g th */ ss adj1 100000
g th2 */ th 1 2
g ext */ ss adj5 200000
g rxo +- wd2 0 ext
g ryo +- hd2 0 ext
g rxi +- rxo 0 th
g ryi +- ryo 0 th
g rxm +- rxo 0 th2
g rym +- ryo 0 th2
g rxe +- rxo ext 0
g rye +- ryo ext 0
g rxf +- rxi 0 ext
g ryf +- ryi 0 ext
g st pin 0 adj4 21599999
g en pin 0 adj3 21599999
g base +- en 0 adj2
g sw0 +- base 0 st
g sw1 +- sw0 21600000 0
g sw ?: sw0 sw0 sw1
g isw +- 0 0 sw
${ellPt("1", "rxo", "ryo", "st")}
${ellPt("2", "rxe", "rye", "base")}
${ellPt("3", "rxm", "rym", "en")}
${ellPt("4", "rxf", "ryf", "base")}
${ellPt("5", "rxi", "ryi", "base")}
M x1 y1 A rxo ryo st sw L x2 y2 L x3 y3 L x4 y4 L x5 y5 A rxi ryi base isw Z`,
  line: "p none; M l t L r b",
  bentConnector2: "p none; M l t L r t L r b",
  bentConnector3: "av adj1 50000; g x1 */ w adj1 100000; p none; M l t L x1 t L x1 b L r b",
  bentConnector4: `
av adj1 50000
av adj2 50000
g x1 */ w adj1 100000
g y2 */ h adj2 100000
p none
M l t L x1 t L x1 y2 L r y2 L r b`,
  bentConnector5: `
av adj1 50000
av adj2 50000
av adj3 50000
g x1 */ w adj1 100000
g x3 */ w adj3 100000
g y2 */ h adj2 100000
p none
M l t L x1 t L x1 y2 L x3 y2 L x3 b L r b`,
  curvedConnector2: "p none; M l t C wd2 t r hd2 r b",
  curvedConnector3: `
av adj1 50000
g x2 */ w adj1 100000
g x1 +/ l x2 2
g x3 +/ r x2 2
g y3 */ h 3 4
p none
M l t C x1 t x2 hd4 x2 vc C x2 y3 x3 b r b`,
  curvedConnector4: `
av adj1 50000
av adj2 50000
g x2 */ w adj1 100000
g x1 +/ l x2 2
g x3 +/ r x2 2
g x4 +/ x2 x3 2
g x5 +/ x3 r 2
g y4 */ h adj2 100000
g y1 +/ t y4 2
g y2 */ y1 1 2
g y3 +/ y1 y4 2
g y5 +/ b y4 2
p none
M l t C x1 t x2 y2 x2 y1 C x2 y3 x4 y4 x3 y4 C x5 y4 r y5 r b`,
  curvedConnector5: `
av adj1 50000
av adj2 50000
av adj3 50000
g x3 */ w adj1 100000
g x6 */ w adj3 100000
g x1 +/ x3 x6 2
g x2 +/ l x3 2
g x4 +/ x3 x1 2
g x5 +/ x6 x1 2
g x7 +/ x6 r 2
g y4 */ h adj2 100000
g y1 +/ t y4 2
g y2 */ y1 1 2
g y3 +/ y1 y4 2
g y5 +/ b y4 2
g y6 +/ y5 y4 2
g y7 +/ y5 b 2
p none
M l t C x2 t x3 y2 x3 y1 C x3 y3 x4 y4 x1 y4 C x5 y4 x6 y6 x6 y5 C x6 y7 x7 b r b`,
};

/** Aliases: same geometry */
const ALIAS: Record<string, string> = {
  flowChartProcess: "rect",
  flowChartDecision: "diamond",
  flowChartConnector: "ellipse",
  straightConnector1: "line",
  bentConnector1: "line",
  curvedConnector1: "line",
  actionButtonBlank: "rect",
  flowChartOfflineStorage: "flowChartMerge",
  flowChartAlternateProcess: "roundRect",
  cloudCallout: "cloud",
  rect: "rect",
};

// ───────────── Code-generated shapes ─────────────

interface Radial {
  /** Number of vertices */
  n: number;
  /** Default adjust value for the star's inner radius (regular polygon if absent) */
  adj?: number;
  hf?: number;
  vf?: number;
  /** Angle of the first vertex (1/60000 degree), defaults to straight up */
  start?: number;
  /** Whether the center shifts down by vf (shapes with an odd number of vertices) */
  svc?: boolean;
}

const RADIAL: Record<string, Radial> = {
  star4: { n: 4, adj: 12500 },
  star5: { n: 5, adj: 19098, hf: 105146, vf: 110557, svc: true },
  star6: { n: 6, adj: 28868, hf: 115470 },
  star7: { n: 7, adj: 34601, hf: 102572, vf: 105210, svc: true },
  star8: { n: 8, adj: 38250 },
  star10: { n: 10, adj: 42533, hf: 105146 },
  star12: { n: 12, adj: 37500 },
  star16: { n: 16, adj: 37500 },
  star24: { n: 24, adj: 37500 },
  star32: { n: 32, adj: 37500 },
  pentagon: { n: 5, hf: 105146, vf: 110557, svc: true },
  heptagon: { n: 7, hf: 102572, vf: 105210, svc: true },
  decagon: { n: 10, vf: 105146, start: 0 },
  dodecagon: { n: 12, hf: 103528, vf: 103528, start: -4500000 },
};

function radialDsl(d: Radial): string {
  const lines: string[] = [];
  const star = d.adj !== undefined;
  if (star) lines.push(`av adj ${d.adj}`, "g a pin 0 adj 50000");
  lines.push(`g swd2 */ wd2 ${d.hf ?? 100000} 100000`, `g shd2 */ hd2 ${d.vf ?? 100000} 100000`);
  lines.push(d.svc ? `g svc */ vc ${d.vf ?? 100000} 100000` : "g svc +- vc 0 0");
  if (star) lines.push("g iwd2 */ swd2 a 50000", "g ihd2 */ shd2 a 50000");
  const count = star ? d.n * 2 : d.n;
  const start = d.start ?? 16200000;
  const cmds: string[] = [];
  for (let i = 0; i < count; i++) {
    const ang = Math.round((start + (i * 21600000) / count) % 21600000);
    const inner = star && i % 2 === 1;
    const rx = inner ? "iwd2" : "swd2";
    const ry = inner ? "ihd2" : "shd2";
    lines.push(`g dx${i} cos ${rx} ${ang}`, `g dy${i} sin ${ry} ${ang}`, `g x${i} +- hc dx${i} 0`, `g y${i} +- svc dy${i} 0`);
    cmds.push(`${i ? "L" : "M"} x${i} y${i}`);
  }
  if (star) {
    // Text rectangle: the inner polygon
    lines.push("g tdx cos iwd2 2700000", "g tdy sin ihd2 2700000", "g til +- hc 0 tdx", "g tir +- hc tdx 0", "g tit +- svc 0 tdy", "g tib +- svc tdy 0", "tx til tit tir tib");
  }
  return `${lines.join("\n")}\n${cmds.join(" ")} Z`;
}

/** Line callouts (callout1–3, borderCallout, accentCallout…): rectangle plus leader lines */
function calloutDsl(name: string): string | null {
  const m = /^(accentBorder|border|accent)?[cC]allout([123])$/.exec(name);
  if (!m) return null;
  const kind = m[1] ?? "";
  const n = Number(m[2]);
  const defaults = [
    [18750, -8333, 112500, -38333],
    [18750, -8333, 18750, -16667, 112500, -46667],
    [18750, -8333, 18750, -16667, 100000, -16667, 112963, -8333],
  ][n - 1];
  const lines = defaults.map((v, i) => `av adj${i + 1} ${v}`);
  for (let i = 0; i < defaults.length; i += 2) {
    const k = i / 2 + 1;
    lines.push(`g y${k} */ h adj${i + 1} 100000`, `g x${k} */ w adj${i + 2} 100000`);
  }
  const border = kind === "border" || kind === "accentBorder";
  lines.push(border ? "p" : "p nostroke", RECT);
  if (kind.startsWith("accent")) lines.push("p none", "M x1 t L x1 b");
  const pts = Array.from({ length: n + 1 }, (_, i) => `${i ? "L" : "M"} x${i + 1} y${i + 1}`);
  lines.push("p none", pts.join(" "));
  return lines.join("\n");
}

const cache = new Map<string, GeomDef | null>();

/** Preset shape name → geometry definition; returns null for unsupported shapes (callers substitute a rectangle) */
export function presetGeom(name: string): GeomDef | null {
  let def = cache.get(name);
  if (def !== undefined) return def;
  const key = ALIAS[name] ?? name;
  const src = SRC[key] ?? (RADIAL[key] ? radialDsl(RADIAL[key]) : calloutDsl(key));
  def = src ? parseDsl(src) : null;
  cache.set(name, def);
  return def;
}

export const RECT_GEOM = parseDsl(RECT);
