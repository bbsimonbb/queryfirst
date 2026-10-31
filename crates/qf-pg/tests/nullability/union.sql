-- expect: id=notnull email=notnull
SELECT id, email FROM customers
UNION
SELECT id, line1 FROM addresses
