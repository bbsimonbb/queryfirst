-- expect: has=notnull nothas=notnull
SELECT EXISTS (SELECT 1 FROM invoices i WHERE i.customer_id = c.id) AS has,
       NOT EXISTS (SELECT 1 FROM invoices i WHERE i.notes IS NULL) AS nothas
FROM customers c
