-- Who may make share links of a space's items: 'editors' (editors, managers and owners, as before), 'managers'
-- (managers and owners) or 'nobody'. Personal spaces keep 'editors': their owner alone decides what is shared.
ALTER TABLE drives ADD COLUMN link_makers TEXT NOT NULL DEFAULT 'editors' CHECK (link_makers IN ('editors', 'managers', 'nobody'));
