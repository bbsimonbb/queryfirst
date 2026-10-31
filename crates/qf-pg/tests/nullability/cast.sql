-- expect: id_text=notnull email=notnull n=nullable:expression
SELECT id::text AS id_text, email::text, CAST(name AS varchar) AS n FROM customers
