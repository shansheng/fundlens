CREATE TABLE IF NOT EXISTS public.fl_sync (
  device text NOT NULL,
  stamp text NOT NULL,
  kind text NOT NULL DEFAULT 'snapshot',
  size integer NOT NULL DEFAULT 0,
  body text NOT NULL DEFAULT '',
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (device, stamp)
);
CREATE INDEX IF NOT EXISTS fl_sync_kind_stamp_idx ON public.fl_sync (kind, stamp);
ALTER TABLE public.fl_sync ENABLE ROW LEVEL SECURITY;
GRANT SELECT, INSERT, UPDATE, DELETE ON public.fl_sync TO service_role;
