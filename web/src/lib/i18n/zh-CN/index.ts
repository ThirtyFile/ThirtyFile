/** Simplified Chinese dictionary, split by feature and merged here (the same files as zh-TW). Keys are the English source text (plurals as "single|plural"); tc() keys are "context::English". */
import server from "./server";
import settings from "./settings";
import spaces from "./spaces";
import users from "./users";
import explorer from "./explorer";
import sharing from "./sharing";
import viewer from "./viewer";
import login from "./login";
import notices from "./notices";
import core from "./core";
import sso from "./sso";
import wording from "./wording";
import accessibility from "./accessibility";
import undo from "./undo";
import largeFolders from "./largeFolders";
import accountSecurity from "./accountSecurity";
import shareLinks from "./shareLinks";
import conflicts from "./conflicts";
import details from "./details";
import dragAndDrop from "./dragAndDrop";
import webdav from "./webdav";
import search from "./search";
import archives from "./archives";
import addressBar from "./addressBar";
import notifications from "./notifications";
import folderSpaces from "./folderSpaces";
import backups from "./backups";
import replicas from "./replicas";

export const LANG = "zh-CN";

export const DICT: Record<string, string> = { ...server, ...settings, ...spaces, ...users, ...explorer, ...sharing, ...viewer, ...login, ...notices, ...core, ...sso, ...wording, ...accessibility, ...undo, ...largeFolders, ...accountSecurity, ...shareLinks, ...conflicts, ...details, ...dragAndDrop, ...webdav, ...search, ...archives, ...addressBar, ...notifications, ...folderSpaces, ...backups, ...replicas };
