-- expect: ids=nullable
/*
 @name byIds
 @param ids -> (...)
*/
SELECT id FROM customers WHERE id IN :ids
