-- expect: email=required:where-eq-notnull name=nullable
SELECT id FROM customers /* é */ WHERE name <> 'Zoë — ✓' AND email = :email AND name = lower(:name)
