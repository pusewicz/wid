module Wid
  module Nodes
    class Identifier < Expression
      alias_method :name, :value

      def ==(other)
        name == other&.name
      end

      def children
        []
      end
    end
  end
end
