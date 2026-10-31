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

INSERT INTO customers (email, name) VALUES ('ada@example.fr', 'Ada'), ('bob@example.fr', NULL);
