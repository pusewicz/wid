module Wid
  class ParserFunctionCallTest < ParserTest
    def test_function_call
      ast = parse("foo(x)")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::CallExpression,
              callee: {
                class: Nodes::Identifier,
                name: "foo"
              },
              arguments: [
                {
                  class: Nodes::Identifier,
                  name: "x"
                }
              ]
            }
          }
        ]
      }, ast)
    end

    def test_function_call_returned_function
      ast = parse("foo(x)(y)")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::CallExpression,
              callee: {
                class: Nodes::CallExpression,
                callee: {
                  class: Nodes::Identifier,
                  name: "foo"
                },
                arguments: [
                  {
                    class: Nodes::Identifier,
                    name: "x"
                  }
                ]
              },
              arguments: [
                {
                  class: Nodes::Identifier,
                  name: "y"
                }
              ]
            }
          }
        ]
      }, ast)
    end

    def test_member_call_function
      ast = parse("foo.bar()")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::CallExpression,
              callee: {
                class: Nodes::MemberExpression,
                computed: false,
                object: {
                  class: Nodes::Identifier,
                  name: "foo"
                },
                property: {
                  class: Nodes::Identifier,
                  name: "bar"
                }
              },
              arguments: []
            }
          }
        ]
      }, ast)
    end
  end
end
