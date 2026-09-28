/** Traditional Chinese dictionary, split by feature and merged here. Keys are the English source text (plurals as "single|plural"); tc() keys are "context::English". */
import server from "./server";
import p1 from "./p1";
import p2 from "./p2";
import p3 from "./p3";
import p4 from "./p4";
import p5 from "./p5";
import p6 from "./p6";
import login from "./login";
import p7 from "./p7";
import core from "./core";
import sso from "./sso";
import p8 from "./p8";
import p9 from "./p9";
import p10 from "./p10";
import p12 from "./p12";
import p13 from "./p13";
import p14 from "./p14";
import p15 from "./p15";
import p16 from "./p16";
import p17 from "./p17";
import p18 from "./p18";
import p19 from "./p19";
import p20 from "./p20";

export const ZH: Record<string, string> = { ...server, ...p1, ...p2, ...p3, ...p4, ...p5, ...p6, ...login, ...p7, ...core, ...sso, ...p8, ...p9, ...p10, ...p12, ...p13, ...p14, ...p15, ...p16, ...p17, ...p18, ...p19, ...p20 };
