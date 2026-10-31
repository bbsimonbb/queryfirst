/** Types generated for queries found in "app/api/(shared-queries)/address.psql" */
import { PreparedQuery } from '@pgtyped/runtime';

/** 'AddressBelongsToAccount' parameters type */
export interface IAddressBelongsToAccountParams {
  accountId?: number | null | void;
  addressId?: number | null | void;
}

/** 'AddressBelongsToAccount' return type */
export interface IAddressBelongsToAccountResult {
  belongs: boolean | null;
}

/** 'AddressBelongsToAccount' query type */
export interface IAddressBelongsToAccountQuery {
  params: IAddressBelongsToAccountParams;
  result: IAddressBelongsToAccountResult;
}

const addressBelongsToAccountIR: any = {"usedParamSet":{"addressId":true,"accountId":true},"params":[{"name":"addressId","required":false,"transform":{"type":"scalar"},"locs":[{"a":114,"b":123}]},{"name":"accountId","required":false,"transform":{"type":"scalar"},"locs":[{"a":145,"b":154}]}],"statement":"SELECT count(*) > 0 as \"belongs\" \nFROM addresses A\ninner join customers C on A.\"customerId\" = C.id\nWHERE A.\"id\" = :addressId and C.\"accountId\" = :accountId"};

/**
 * Query generated from SQL:
 * ```
 * SELECT count(*) > 0 as "belongs" 
 * FROM addresses A
 * inner join customers C on A."customerId" = C.id
 * WHERE A."id" = :addressId and C."accountId" = :accountId
 * ```
 */
export const addressBelongsToAccount = new PreparedQuery<IAddressBelongsToAccountParams,IAddressBelongsToAccountResult>(addressBelongsToAccountIR);


