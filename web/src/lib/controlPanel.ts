import { useNavigate } from "react-router";
import {
  ActivityIcon,
  ArchiveIcon,
  DatabaseIcon,
  HardDriveIcon,
  KeyRoundIcon,
  PaletteIcon,
  PieChartIcon,
  SlidersHorizontalIcon,
  UsersIcon,
  UsersRoundIcon,
  type LucideIcon,
} from "lucide-react";
import { t, tc } from "@/lib/i18n";

export type ControlPanelKey = "users" | "groups" | "drives" | "storage" | "usage" | "general" | "branding" | "sso" | "activity" | "logs";

export interface ControlPanelItem {
  key: ControlPanelKey;
  to: string;
  title: string;
  desc: string;
  icon: LucideIcon;
  /** Icon background and foreground colors */
  tone: string;
  category: ControlPanelCategory;
  /** Extra keywords for search */
  keywords: string;
}

export type ControlPanelCategory = "users" | "storage" | "system";

/** Control panel categories (id is used for matching and grouping, label is the display text) */
export const CONTROL_PANEL_CATEGORIES: { id: ControlPanelCategory; label: string }[] = [
  { id: "users", label: t("Users and permissions") },
  { id: "storage", label: t("Spaces and storage") },
  { id: "system", label: t("System") },
];

export const controlPanelCategoryLabel = (id: ControlPanelCategory) => CONTROL_PANEL_CATEGORIES.find((c) => c.id === id)?.label ?? id;

export const CONTROL_PANEL_ITEMS: ControlPanelItem[] = [
  {
    key: "users",
    to: "/admin/users",
    title: t("Users"),
    desc: t("Add or disable accounts; set permissions, quotas, and administrators"),
    icon: UsersIcon,
    tone: "bg-sky-500/12 text-sky-600 dark:text-sky-300",
    category: "users",
    keywords: "帳號 密碼 管理員 權限 配額 容量 user account password admin administrator permission quota", // i18n-ignore: bilingual search keywords
  },
  {
    key: "groups",
    to: "/admin/groups",
    title: t("Groups"),
    desc: t("Organize users into groups and grant a whole group access to spaces at once"),
    icon: UsersRoundIcon,
    tone: "bg-violet-500/12 text-violet-600 dark:text-violet-300",
    category: "users",
    keywords: "成員 授權 group member access", // i18n-ignore: bilingual search keywords
  },
  {
    key: "drives",
    to: "/admin/drives",
    title: tc("admin", "Spaces"),
    desc: t("Team space members, quotas, and storage locations"),
    icon: HardDriveIcon,
    tone: "bg-emerald-500/12 text-emerald-700 dark:text-emerald-300",
    category: "storage",
    keywords: "團隊 個人 配額 搬移 封存 drive space team personal quota move archive", // i18n-ignore: bilingual search keywords
  },
  {
    key: "storage",
    to: "/admin/storage",
    title: t("Storage locations"),
    desc: t("Local folders, NAS, or S3-compatible object storage (AWS, R2, RustFS)"),
    icon: DatabaseIcon,
    tone: "bg-amber-500/12 text-amber-700 dark:text-amber-300",
    category: "storage",
    keywords: "s3 r2 aws minio rustfs nas 本機 磁碟 bucket 預設 storage location local disk default", // i18n-ignore: bilingual search keywords
  },
  {
    key: "usage",
    to: "/admin/usage",
    title: t("Storage usage"),
    desc: t("Usage by space type, trash, and actual disk usage"),
    icon: PieChartIcon,
    tone: "bg-rose-500/12 text-rose-600 dark:text-rose-300",
    category: "storage",
    keywords: "統計 容量 垃圾桶 分享連結 usage statistics capacity trash share link", // i18n-ignore: bilingual search keywords
  },
  {
    key: "general",
    to: "/admin/general",
    title: t("General"),
    desc: t("Site URL, default language, the \"All files\" company space, who can create team spaces, and the default space size for new users"),
    icon: SlidersHorizontalIcon,
    tone: "bg-brand/12 text-brand",
    category: "system",
    keywords: "網址 網域 url domain 分享連結 全部檔案 公司 共用 團隊空間 建立 新使用者 預設 空間大小 配額 容量 語言 中文 英文 預設語言 兩步驟驗證 驗證碼 密碼長度 安全 general site address share link all files company shared team space create new user default size quota language english chinese two-factor 2fa totp authenticator password length security", // i18n-ignore: bilingual search keywords
  },
  {
    key: "branding",
    to: "/admin/branding",
    title: t("Branding"),
    desc: t("Site name, logo, theme colors, light/dark mode, and sign-in page text"),
    icon: PaletteIcon,
    tone: "bg-pink-500/12 text-pink-600 dark:text-pink-300",
    category: "system",
    keywords: "品牌 logo 標誌 圖示 名稱 標題 配色 顏色 主題 主色 深色 淺色 暗色 亮色 外觀 登入頁 歡迎 鎖定畫面 時鐘 背景 桌布 favicon branding icon name title color theme accent dark light mode appearance sign-in page welcome lock screen clock background wallpaper", // i18n-ignore: bilingual search keywords
  },
  {
    key: "sso",
    to: "/admin/sso",
    title: t("Single sign-on"),
    desc: t("Sign in with Microsoft Entra ID, Google, or GitHub accounts"),
    icon: KeyRoundIcon,
    tone: "bg-amber-500/12 text-amber-600 dark:text-amber-300",
    category: "users",
    keywords: "sso 單一登入 三方登入 第三方 oauth oidc microsoft entra azure ad office 365 google workspace github 登入 帳號 連結 single sign-on third-party sign in login account link", // i18n-ignore: bilingual search keywords
  },
  {
    key: "activity",
    to: "/admin/activity",
    title: t("Activity log"),
    desc: t("Search user actions, sign-ins, and share link access; filter and export"),
    icon: ActivityIcon,
    tone: "bg-slate-500/12 text-slate-600 dark:text-slate-300",
    category: "system",
    keywords: "紀錄 稽核 log 查詢 篩選 匯出 csv 分享連結 存取 下載 ip 登入 登出 密碼 失敗 鎖定 activity audit search filter export share link access download sign in sign out login logout password failed locked", // i18n-ignore: bilingual search keywords
  },
  {
    key: "logs",
    to: "/admin/logs",
    title: t("Log settings"),
    desc: t("How long logs are kept, whether older logs are archived or deleted, and whether visitor IPs are recorded"),
    icon: ArchiveIcon,
    tone: "bg-orange-500/12 text-orange-600 dark:text-orange-300",
    category: "system",
    keywords: "log 保留 封存 壓縮 清理 天數 備份 ip 個資 logs settings retention archive compress cleanup days backup privacy personal data", // i18n-ignore: bilingual search keywords
  },
];

export const controlPanelItem = (key: ControlPanelKey) => CONTROL_PANEL_ITEMS.find((i) => i.key === key)!;

/** Search box on settings pages: typing returns to the control panel and filters the settings */
export function useSettingsSearch() {
  const navigate = useNavigate();
  return (q: string) => {
    if (q.trim()) navigate(`/admin?q=${encodeURIComponent(q)}`);
  };
}
