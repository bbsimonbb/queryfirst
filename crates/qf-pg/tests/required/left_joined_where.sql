-- expect: line1=nullable
SELECT c.id FROM customers c LEFT JOIN addresses a ON a.customer_id = c.id WHERE a.line1 = :line1
