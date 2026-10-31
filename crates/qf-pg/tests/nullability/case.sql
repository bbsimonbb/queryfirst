-- expect: label=notnull e=nullable:expression f=nullable:expression
SELECT CASE WHEN name IS NULL THEN 'anon' ELSE email END AS label,
       CASE WHEN id > 1 THEN email END AS e,
       CASE access WHEN 'admin' THEN name ELSE email END AS f
FROM customers
