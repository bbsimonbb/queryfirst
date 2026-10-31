-- expect: line1=required:where-eq-notnull
DELETE FROM customers c USING addresses a WHERE a.customer_id = c.id AND a.line1 = :line1
