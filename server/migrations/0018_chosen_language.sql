-- The interface language a person chose themselves (en, zh-TW, zh-CN or ja), kept with the account so it follows
-- them to every device; '' = none chosen. `lang` keeps the language they last used, which may also have come from the
-- system default or the browser (i18n/mod.rs).
ALTER TABLE users ADD COLUMN chosen_lang TEXT NOT NULL DEFAULT '';
