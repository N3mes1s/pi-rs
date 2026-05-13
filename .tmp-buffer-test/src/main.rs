use futures::stream::{self, StreamExt};

#[tokio::main]
async fn main() {
    let fut = async {
        let v: Vec<_> = stream::iter(vec![1,2,3])
            .map(|x| async move { x })
            .buffer_unordered(0)
            .collect()
            .await;
        println!("{:?}", v);
    };
    match tokio::time::timeout(std::time::Duration::from_secs(2), fut).await {
        Ok(_) => println!("done"),
        Err(_) => println!("timeout"),
    }
}
