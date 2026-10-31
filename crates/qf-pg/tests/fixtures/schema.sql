-- Test schema for qf-pg's db-tests. Loaded into a fresh, uniquely named schema via search_path,
-- so it must not schema-qualify anything.

CREATE TYPE "accessLevel" AS ENUM ('user', 'admin', 'owner');

CREATE TABLE customers (
    id serial PRIMARY KEY,
    email varchar(30) NOT NULL,
    name text,
    access "accessLevel" NOT NULL DEFAULT 'user',
    tags text[],
    prefs jsonb,
    created_at timestamp NOT NULL DEFAULT now(),
    birthday date
);

CREATE TABLE addresses (
    id serial PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES customers (id),
    line1 text NOT NULL,
    city text,
    postcode varchar(30)
);

CREATE TABLE invoices (
    id serial PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES customers (id),
    address_id integer REFERENCES addresses (id),
    total numeric(12,2) NOT NULL DEFAULT 0,
    issued_on date NOT NULL DEFAULT current_date,
    notes text
);

-- Row hooks that may replace a NULL before NOT NULL is checked (required inference must not trust
-- these NOT NULL targets): a BEFORE INSERT trigger on posts, an ON UPDATE rule on drafts.
CREATE TABLE posts (
    id serial PRIMARY KEY,
    title text NOT NULL,
    slug text NOT NULL
);

CREATE FUNCTION posts_fill_slug() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.slug := coalesce(NEW.slug, lower(NEW.title));
    RETURN NEW;
END
$$;

CREATE TRIGGER posts_fill_slug BEFORE INSERT ON posts
    FOR EACH ROW EXECUTE FUNCTION posts_fill_slug();

CREATE TABLE drafts (
    id serial PRIMARY KEY,
    body text NOT NULL
);

CREATE RULE drafts_notify AS ON UPDATE TO drafts DO ALSO NOTIFY drafts;

INSERT INTO customers (email, name) VALUES ('ada@example.fr', 'Ada'), ('bob@example.fr', NULL);
