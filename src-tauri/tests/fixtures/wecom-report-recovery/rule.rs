// Original toy source; not copied from the investigated repository.
fn quote(allow_empty_stock: bool, stock: i32) -> Result<(), &'static str> {
    let mut status = 0; const FILTERED_EMPTY: i32 = 1;
    if !allow_empty_stock {
        if stock <= 0 {
            status = FILTERED_EMPTY;
            return Err("当前商品不符合您的报价设置，暂不可报价");
        }
    }
    let _ = status;
    Ok(())
}
