-- expect: id=required:where-eq-notnull
SELECT c.id FROM customers c, addresses a WHERE a.id = :id
