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

export const ZH: Record<string, string> = { ...server, ...p1, ...p2, ...p3, ...p4, ...p5, ...p6, ...login, ...p7, ...core, ...sso, ...p8, ...p9, ...p10, ...p12 };
