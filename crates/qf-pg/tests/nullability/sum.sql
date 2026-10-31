-- expect: s=nullable:expression m=nullable:expression ids=nullable:expression
SELECT sum(total) AS s, max(id) AS m, array_agg(id) AS ids FROM invoices
