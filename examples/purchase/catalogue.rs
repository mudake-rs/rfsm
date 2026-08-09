#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductId(&'static str);

impl ProductId {
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Product {
    Miner { id: ProductId },
    Vip { id: ProductId },
    Diamonds { id: ProductId, amount: u64 },
}

pub const MINER_CURRENT: ProductId = ProductId("com.example.mining.3.three_months");
pub const MINER_RETIRED: ProductId = ProductId("com.example.miners.3.three_months");
pub const VIP: ProductId = ProductId("com.example.vip");
pub const DIAMONDS: ProductId = ProductId("com.example.diamonds.2000000");

pub fn lookup(product_id: &str) -> Option<Product> {
    match product_id {
        "com.example.mining.3.three_months" => Some(Product::Miner { id: MINER_CURRENT }),
        "com.example.miners.3.three_months" => Some(Product::Miner { id: MINER_RETIRED }),
        "com.example.vip" => Some(Product::Vip { id: VIP }),
        "com.example.diamonds.2000000" => Some(Product::Diamonds {
            id: DIAMONDS,
            amount: 2_000_000,
        }),
        _ => None,
    }
}
