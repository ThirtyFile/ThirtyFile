// The web app's API types against the shapes of the server's responses (shapes.json, written by the server's test
// `api_shapes_match_the_recorded_ones`). Type-checking this file (pnpm typecheck) fails when a type requires a field
// the server doesn't send, or the server sends a field the type doesn't have; the error names the fields.

import { expect, test } from "vitest";
import type {
  AccessInfo,
  AlertsInfo,
  AppPassword,
  ContentSearchInfo,
  Activity,
  Device,
  Drive,
  EmailSettings,
  Grant,
  Group,
  GuestConversionPreview,
  HistoryEntry,
  Job,
  Located,
  LogStatus,
  Me,
  NewAppPassword,
  Node,
  NodeInfo,
  NotificationSettings,
  Principal,
  RestrictionPreview,
  ShareInfo,
  SharedItem,
  SsoSettings,
  SystemInfo,
  Tag,
  TwoFactorStatus,
  UserRow,
} from "@/api/types";
import type { FtpCreated, FtpFolder, FtpStatus } from "@/api/ftp";
import type { NewS3Bucket, NewS3Key, S3Bucket, S3Config, S3Key } from "@/api/s3";
import type { SftpKey, SftpSettings } from "@/api/sftp";
import shapes from "../contract/shapes.json";

type Shapes = typeof shapes;

/** The fields a type can't do without */
type RequiredKeys<T> = { [K in keyof T]-?: object extends Pick<T, K> ? never : K }[keyof T];

/** How a response and its type differ: fields the type requires that the response lacks, and fields the type lacks */
type Drift<Response, T> = { missing: Exclude<RequiredKeys<T>, keyof Response>; unknown: Exclude<keyof Response, keyof T> };

/** Compiles only when they agree; otherwise the error shows `Drift` with the fields' names */
function agrees<D extends { missing: never; unknown: never }>(): D | undefined {
  return undefined;
}

/** Each recorded response, checked against the type the app reads it as */
const checked: { [K in keyof Shapes]: unknown } = {
  AccessInfo: agrees<Drift<Shapes["AccessInfo"], AccessInfo>>(),
  AlertsInfo: agrees<Drift<Shapes["AlertsInfo"], AlertsInfo>>(),
  ContentSearchInfo: agrees<Drift<Shapes["ContentSearchInfo"], ContentSearchInfo>>(),
  AppPassword: agrees<Drift<Shapes["AppPassword"], AppPassword>>(),
  Activity: agrees<Drift<Shapes["Activity"], Activity>>(),
  Device: agrees<Drift<Shapes["Device"], Device>>(),
  Drive: agrees<Drift<Shapes["Drive"], Drive>>(),
  EmailSettings: agrees<Drift<Shapes["EmailSettings"], EmailSettings>>(),
  FtpCreated: agrees<Drift<Shapes["FtpCreated"], FtpCreated>>(),
  FtpFolder: agrees<Drift<Shapes["FtpFolder"], FtpFolder>>(),
  FtpStatus: agrees<Drift<Shapes["FtpStatus"], FtpStatus>>(),
  Grant: agrees<Drift<Shapes["Grant"], Grant>>(),
  Group: agrees<Drift<Shapes["Group"], Group>>(),
  GuestConversionPreview: agrees<Drift<Shapes["GuestConversionPreview"], GuestConversionPreview>>(),
  HistoryEntry: agrees<Drift<Shapes["HistoryEntry"], HistoryEntry>>(),
  Job: agrees<Drift<Shapes["Job"], Job>>(),
  Located: agrees<Drift<Shapes["Located"], Located>>(),
  LogStatus: agrees<Drift<Shapes["LogStatus"], LogStatus>>(),
  Me: agrees<Drift<Shapes["Me"], Me>>(),
  NewAppPassword: agrees<Drift<Shapes["NewAppPassword"], NewAppPassword>>(),
  NewS3Bucket: agrees<Drift<Shapes["NewS3Bucket"], NewS3Bucket>>(),
  NewS3Key: agrees<Drift<Shapes["NewS3Key"], NewS3Key>>(),
  Node: agrees<Drift<Shapes["Node"], Node>>(),
  NodeInfo: agrees<Drift<Shapes["NodeInfo"], NodeInfo>>(),
  NotificationSettings: agrees<Drift<Shapes["NotificationSettings"], NotificationSettings>>(),
  Principal: agrees<Drift<Shapes["Principal"], Principal>>(),
  RestrictionPreview: agrees<Drift<Shapes["RestrictionPreview"], RestrictionPreview>>(),
  S3Bucket: agrees<Drift<Shapes["S3Bucket"], S3Bucket>>(),
  S3Config: agrees<Drift<Shapes["S3Config"], S3Config>>(),
  S3Key: agrees<Drift<Shapes["S3Key"], S3Key>>(),
  SftpKey: agrees<Drift<Shapes["SftpKey"], SftpKey>>(),
  SftpSettings: agrees<Drift<Shapes["SftpSettings"], SftpSettings>>(),
  ShareInfo: agrees<Drift<Shapes["ShareInfo"], ShareInfo>>(),
  SharedItem: agrees<Drift<Shapes["SharedItem"], SharedItem>>(),
  SsoSettings: agrees<Drift<Shapes["SsoSettings"], SsoSettings>>(),
  SystemInfo: agrees<Drift<Shapes["SystemInfo"], SystemInfo>>(),
  Tag: agrees<Drift<Shapes["Tag"], Tag>>(),
  TwoFactorStatus: agrees<Drift<Shapes["TwoFactorStatus"], TwoFactorStatus>>(),
  UserRow: agrees<Drift<Shapes["UserRow"], UserRow>>(),
};

test("every recorded response is checked against a type", () => {
  expect(Object.keys(checked).sort()).toEqual(Object.keys(shapes).sort());
});
