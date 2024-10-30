module Wid
  class ParserClassTest < ParserTest
    def test_class
      ast = parse(<<~WID)
        class Point
          def initialize(x, y)
            self.x = x
            self.y = y
          end

          def calc()
            return self.x + self.y
          end
        end
      WID

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ClassDeclaration,
            name: {
              class: Nodes::Identifier,
              name: "Point"
            },
            superclass: nil,
            body: {
              class: Nodes::BlockStatement,
              body: [
                {
                  class: Nodes::FunctionDeclaration,
                  name: {
                    class: Nodes::Identifier,
                    name: "initialize"
                  },
                  params: [
                    {
                      class: Nodes::Identifier,
                      name: "x"
                    },
                    {
                      class: Nodes::Identifier,
                      name: "y"
                    }
                  ],
                  body: {
                    class: Nodes::BlockStatement,
                    body: [
                      {
                        class: Nodes::ExpressionStatement,
                        expression: {
                          class: Nodes::AssignmentExpression,
                          operator: "=",
                          left: {
                            class: Nodes::MemberExpression,
                            computed: false,
                            object: {
                              class: Nodes::SelfExpression
                            },
                            property: {
                              class: Nodes::Identifier,
                              name: "x"
                            }
                          },
                          right: {
                            class: Nodes::Identifier,
                            name: "x"
                          }
                        }
                      },
                      {
                        class: Nodes::ExpressionStatement,
                        expression: {
                          class: Nodes::AssignmentExpression,
                          operator: "=",
                          left: {
                            class: Nodes::MemberExpression,
                            computed: false,
                            object: {
                              class: Nodes::SelfExpression
                            },
                            property: {
                              class: Nodes::Identifier,
                              name: "y"
                            }
                          },
                          right: {
                            class: Nodes::Identifier,
                            name: "y"
                          }
                        }
                      }
                    ]
                  }
                },
                {
                  class: Nodes::FunctionDeclaration,
                  name: {
                    class: Nodes::Identifier,
                    name: "calc"
                  },
                  params: [],
                  body: {
                    class: Nodes::BlockStatement,
                    body: [
                      {
                        class: Nodes::ReturnStatement,
                        argument: {
                          class: Nodes::BinaryExpression,
                          operator: "+",
                          left: {
                            class: Nodes::MemberExpression,
                            computed: false,
                            object: {
                              class: Nodes::SelfExpression
                            },
                            property: {
                              class: Nodes::Identifier,
                              name: "x"
                            }
                          },
                          right: {
                            class: Nodes::MemberExpression,
                            computed: false,
                            object: {
                              class: Nodes::SelfExpression
                            },
                            property: {
                              class: Nodes::Identifier,
                              name: "y"
                            }
                          }
                        }
                      }
                    ]
                  }
                }
              ]
            }
          }
        ]
      }, ast)
    end

    def test_class_inheritance_with_super
      ast = parse(<<~WID)
        class Point3D < Point
          def initialize(x, y, z)
            super(x, y)
            self.z = z
          end

          def calc()
            return super() + self.z
          end
        end
      WID

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ClassDeclaration,
            name: {
              class: Nodes::Identifier,
              name: "Point3D"
            },
            superclass: {
              class: Nodes::Identifier,
              name: "Point"
            },
            body: {
              class: Nodes::BlockStatement,
              body: [
                {
                  class: Nodes::FunctionDeclaration,
                  name: {
                    class: Nodes::Identifier,
                    name: "initialize"
                  },
                  params: [
                    {
                      class: Nodes::Identifier,
                      name: "x"
                    },
                    {
                      class: Nodes::Identifier,
                      name: "y"
                    },
                    {
                      class: Nodes::Identifier,
                      name: "z"
                    }
                  ],
                  body: {
                    class: Nodes::BlockStatement,
                    body: [
                      {
                        class: Nodes::ExpressionStatement,
                        expression: {
                          class: Nodes::CallExpression,
                          callee: {
                            class: Nodes::SuperExpression
                          },
                          arguments: [
                            {
                              class: Nodes::Identifier,
                              name: "x"
                            },
                            {
                              class: Nodes::Identifier,
                              name: "y"
                            }
                          ]
                        }
                      },
                      {
                        class: Nodes::ExpressionStatement,
                        expression: {
                          class: Nodes::AssignmentExpression,
                          operator: "=",
                          left: {
                            class: Nodes::MemberExpression,
                            computed: false,
                            object: {
                              class: Nodes::SelfExpression
                            },
                            property: {
                              class: Nodes::Identifier,
                              name: "z"
                            }
                          },
                          right: {
                            class: Nodes::Identifier,
                            name: "z"
                          }
                        }
                      }
                    ]
                  }
                },
                {
                  class: Nodes::FunctionDeclaration,
                  name: {
                    class: Nodes::Identifier,
                    name: "calc"
                  },
                  params: [],
                  body: {
                    class: Nodes::BlockStatement,
                    body: [
                      {
                        class: Nodes::ReturnStatement,
                        argument: {
                          class: Nodes::BinaryExpression,
                          operator: "+",
                          left: {
                            class: Nodes::CallExpression,
                            callee: {
                              class: Nodes::SuperExpression
                            },
                            arguments: []
                          },
                          right: {
                            class: Nodes::MemberExpression,
                            computed: false,
                            object: {
                              class: Nodes::SelfExpression
                            },
                            property: {
                              class: Nodes::Identifier,
                              name: "z"
                            }
                          }
                        }
                      }
                    ]
                  }
                }
              ]
            }
          }
        ]
      }, ast)
    end
  end
end
