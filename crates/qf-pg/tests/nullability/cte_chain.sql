-- expect: email=notnull line1=nullable:outer-join
WITH a AS (SELECT c.id, c.email FROM customers c),
     b AS (SELECT a.email, ad.line1 FROM a LEFT JOIN addresses ad ON ad.customer_id = a.id)
SELECT email, line1 FROM b
